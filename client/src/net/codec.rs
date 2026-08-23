//! The one place untrusted bytes become values.
//!
//! Mirrors `server/internal/protocol`: everything the client needs is copied out
//! of the frame into plain Rust values, so no accessor over bytes a peer chose
//! ever escapes this module. The Rust FlatBuffers runtime ships a verifier — so
//! [`decode`] returns an error where the server has to recover from a panic — but
//! the copying rule stands for a second reason anyway: an accessor borrows the
//! frame, and frames are transient.
//!
//! `ServerWelcome` is validated here, before its numbers exist as a value at all.
//! That is deliberate: [`SessionParams`] can only be constructed by this module,
//! so there is no reachable state in which the rest of the client holds a
//! `tick_rate` of zero to divide by or a `view_distance` of 255 to allocate from.

use std::collections::HashSet;
use std::fmt;

use flatbuffers::FlatBufferBuilder;

use crate::wire::voxelheim::net as fb;

/// Largest chunk edge the contract can address, mirroring
/// `protocol.MaxChunkSize`: a single RLE run length is a `u16`, and 40³ (64000)
/// is the last cube that fits.
pub const MAX_CHUNK_SIZE: u16 = 40;

/// Bounds the streamed volume, which grows as `(2r + 1)³` chunks. Mirrors
/// `protocol.MaxViewDistance`.
pub const MAX_VIEW_DISTANCE: u8 = 16;

/// Length of an identity token, in bytes. Fixed by `schemas/handshake.fbs`: a
/// `player_token` is absent, empty, or exactly this — nothing else is a token.
pub const PLAYER_TOKEN_LEN: usize = 32;

/// Length of a session ticket, in bytes. Fixed by `schemas/handshake.fbs` from V7:
/// a `session_ticket` is absent, empty, or exactly this — nothing else is a ticket.
/// Mirrors `protocol.SessionTicketLen`.
pub const SESSION_TICKET_LEN: usize = 96;

/// The neutral grey an entity is drawn in while its `PlayerAppearance` has not
/// arrived yet — `0x808080` for every colour, with [`HairModel::Shaved`].
///
/// **A rendering placeholder and never a decoding default.** `schemas/player.fbs`
/// documents it for exactly one case: the two streams are not ordered against each
/// other, so a player can be visible for a frame or two before the message describing
/// them lands. An `Appearance` that *did* arrive and broke an invariant is refused
/// instead — the client may not invent what the server actually described.
///
/// Built through [`Appearance::new`] like every other appearance in this build, which is
/// what makes "the placeholder is a legal appearance" a compile error rather than a
/// sentence: the `Err` arm below cannot be reached, and the constructor is what says so.
// The reader arrived with the bodies: `player::apply_snapshots` dresses an entity in this
// when no `PlayerAppearance` for it has landed yet, and `player::dress_bodies` replaces it
// in place the moment one does. The character screen still needs none — every appearance
// it draws is either one the server listed or one the player is choosing.
pub const PLACEHOLDER_APPEARANCE: Appearance = match Appearance::new(
    0x0080_8080,
    0x0080_8080,
    0x0080_8080,
    0x0080_8080,
    HairModel::Shaved,
    0x0080_8080,
) {
    Ok(appearance) => appearance,
    Err(_) => panic!("the placeholder is not an appearance this contract allows"),
};

/// Initial builder capacity for the client's small intent messages. A hello,
/// input or request fits without reallocating.
const BUILDER_CAPACITY: usize = 128;

/// Fallback for a payload or reject code this build has no name for — a peer
/// speaking a newer contract than `ProtocolVersion::Current` describes.
pub(super) const UNKNOWN_VARIANT: &str = "UNKNOWN";

/// A server-minted identity token, exactly [`PLAYER_TOKEN_LEN`] bytes.
///
/// A newtype rather than a bare array for one reason, and it is the reason
/// `schemas/handshake.fbs` gives: this is a bearer credential. Whatever can read
/// it can be the player it names, so the contract's rule is *never logged, never
/// displayed* — and the one place that rule is easy to break by accident is a
/// `{:?}` of a struct that happens to contain one. [`fmt::Debug`] is therefore
/// written by hand and prints no bytes at all, which makes the redaction a
/// property of the type instead of a habit every call site has to remember.
///
/// There is no `Display`, deliberately: a token has no rendering, so the one
/// trait that would put it on a screen simply does not exist for it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PlayerToken([u8; PLAYER_TOKEN_LEN]);

impl PlayerToken {
    /// Wraps bytes that are already known to be a token.
    pub const fn from_bytes(bytes: [u8; PLAYER_TOKEN_LEN]) -> Self {
        Self(bytes)
    }

    /// The bytes, for the two callers that must have them: the encoder that puts
    /// the token back on the wire, and the file it is stored in between sessions.
    /// Nothing else has a use for them.
    pub const fn as_bytes(&self) -> &[u8; PLAYER_TOKEN_LEN] {
        &self.0
    }
}

/// A token for the many tests that need a [`SessionParams`] and have no opinion
/// about which identity it names.
///
/// Test-only, and deliberately not a `Default` impl: there is no such thing as a
/// default identity, only one a particular test does not care about. Every test
/// that *is* about identity names its own bytes.
#[cfg(test)]
pub const ANY_TOKEN: PlayerToken = PlayerToken::from_bytes([0x11; PLAYER_TOKEN_LEN]);

impl fmt::Debug for PlayerToken {
    /// Redacted, always. See the type's documentation: this is the whole point of
    /// the newtype, and a derived `Debug` would put a bearer credential into
    /// every log line, panic message and assertion failure that touches a
    /// [`SessionParams`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlayerToken(<redacted>)")
    }
}

/// A signed session ticket, exactly [`SESSION_TICKET_LEN`] bytes.
///
/// A newtype for the reason [`PlayerToken`] is one, and the reason is unchanged by
/// the signature: this is a bearer credential. A signature proves who *issued* the
/// ticket, not who is holding it, so a copy taken off the wire is as good as the
/// original — which makes `schemas/handshake.fbs`'s rule the same rule as before,
/// *never logged, never displayed*. [`fmt::Debug`] is written by hand and prints no
/// bytes, so the redaction is a property of the type rather than a habit every call
/// site has to remember, and there is deliberately no `Display`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionTicket([u8; SESSION_TICKET_LEN]);

impl SessionTicket {
    /// Wraps bytes that are already known to be a ticket — decoded from what the
    /// account service answered with, or read back out of the cache beside it.
    pub const fn from_bytes(bytes: [u8; SESSION_TICKET_LEN]) -> Self {
        Self(bytes)
    }

    /// The bytes, for the two callers that must have them: the encoder that puts the
    /// ticket on the wire, and the cache that keeps one between launches. Nothing in
    /// this client reads a ticket's *contents* — the account named, the world named
    /// and the expiry signed into it are the account service's to state and the game
    /// server's to check, and `internal/ticket` is explicit that nothing outside it
    /// parses a ticket.
    pub const fn as_bytes(&self) -> &[u8; SESSION_TICKET_LEN] {
        &self.0
    }
}

impl fmt::Debug for SessionTicket {
    /// Redacted, always. See the type's documentation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionTicket(<redacted>)")
    }
}

/// The authoritative session parameters from a **validated** `ServerWelcome`.
///
/// Every invariant `schemas/handshake.fbs` documents has already been checked by
/// the time one of these exists. `Copy` because it is a handful of scalars that
/// every later system will want to read without ceremony.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionParams {
    /// Server-assigned identity for this player's entity.
    pub entity_id: u64,
    /// Where the server has placed the player. The client renders where the
    /// snapshots say; this is the starting answer, not a claim the client makes.
    pub spawn: [f32; 3],
    /// Seed of the world's procedural generation — diagnostics only. The client
    /// never generates terrain from it, because the server's voxels are the only
    /// voxels that exist.
    pub world_seed: i64,
    /// Authoritative simulation ticks per second. Guaranteed `>= 1`.
    pub tick_rate: u8,
    /// Edge length of a chunk in blocks. Guaranteed `1..=MAX_CHUNK_SIZE`.
    pub chunk_size: u16,
    /// Chunk-streaming radius in chunks. Guaranteed `<= MAX_VIEW_DISTANCE`.
    pub view_distance: u8,
    /// Number of slot pairs in every InventoryState. Guaranteed non-zero.
    pub inventory_slots: u8,
    /// Leading inventory slots selectable as the hotbar. Guaranteed non-zero
    /// and no greater than `inventory_slots`.
    pub hotbar_slots: u8,
    /// This session's identity token. Guaranteed to have been present and exactly
    /// [`PLAYER_TOKEN_LEN`] bytes on the wire.
    ///
    /// Carried here because the client stores it and presents it again next time,
    /// not because anything is decided from it: the server settled the identity
    /// before it sent this, and no client-side branch reads the value.
    pub player_token: PlayerToken,
    /// The world's day/night boundaries, or [`WorldClock::default`] — three zeros —
    /// from a server that keeps no clock.
    pub clock: WorldClock,
}

/// The world's day, as `ServerWelcome` announces it.
///
/// **One type rather than three fields, because the three numbers are one fact and the
/// first of them decides whether the other two mean anything.** A caller that read
/// `night_start_ticks` without first asking whether a clock exists would get a zero and
/// no way to tell it from a night that begins at midnight; [`Self::declared`] is that
/// question, and having to call it is the point.
///
/// The server-side counterpart is three flat fields on `protocol.Welcome`, deliberately:
/// there they are written by the simulation that owns the constants, and here they are
/// untrusted input that has to be validated together. Grouping is worth a type on the
/// side that has to ask.
///
/// **[`Default`] is the absence of a clock** — three zeros, which is both the pre-V6
/// world and what every server in this repository announces today. There is deliberately
/// no second spelling of that value: one way to say "no clock" is one way for it to be
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorldClock {
    /// Length of one full day in server ticks. **Zero means this server keeps no
    /// clock** — the world as it was before V6, in which time does not pass.
    ///
    /// A legal, expected announcement rather than a missing value, and the one number
    /// in [`SessionParams`] allowed to be zero after validation. Nothing divides by it
    /// without asking [`Self::declared`] first.
    pub day_length_ticks: u32,
    /// The ticks at which night begins and ends within the day.
    ///
    /// Meaningful only when [`Self::declared`], and then guaranteed to satisfy
    /// `0 < night_start_ticks < night_end_ticks <= day_length_ticks` — the decoder
    /// refuses a welcome that says otherwise rather than repairing it. Both are zero
    /// when there is no clock, which is the same nothing `day_length_ticks` reports and
    /// is never a night that begins and ends at dawn.
    pub night_start_ticks: u32,
    pub night_end_ticks: u32,
}

impl WorldClock {
    /// Whether this world has a time of day at all.
    ///
    /// The question every reader of the two boundaries has to ask first, and the reason
    /// they are not three loose fields on [`SessionParams`].
    pub fn declared(&self) -> bool {
        self.day_length_ticks != 0
    }
}

/// A chunk's address in chunk units, copied out of a `ChunkCoord` struct.
///
/// Mirrors `world.Coord` on the server, and is the key every chunk is stored
/// under. Multiply by [`SessionParams::chunk_size`] for the world coordinate of
/// the chunk's minimum corner — the schema says so, and it is the server's number
/// rather than a constant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkCoord {
    pub cx: i32,
    pub cy: i32,
    pub cz: i32,
}

/// One voxel's address in **world** coordinates, measured in blocks.
///
/// World coordinates and not chunk-plus-local, because that is what the contract
/// carries: `schemas/world.fbs` puts the conversion on the server so there is only
/// one copy of that arithmetic, and the untrusted side is not it. The client does
/// the same split for its own store — see `world::locate` — but what crosses the
/// wire is the point, in both directions.
///
/// Signed on all three axes, the vertical one included: the world extends in both
/// directions and the origin is not the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockCoord {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// What a player is asking to do to a voxel.
///
/// The wire enum's `Unknown = 0` is deliberately **not** a variant here. It exists
/// on the wire so that a `BlockEditRequest` with no `action` fails closed on the
/// server; on this side it would only be a value every encoder had to promise never
/// to send. Making it unrepresentable is the stronger promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    /// Remove the block at the target. What is left behind is the server's answer.
    // Kept so the codec test continues to pin Protocol V1's existing enum value;
    // production mining now uses MineRequest and never emits this legacy action.
    #[allow(dead_code)]
    Break,
    /// Put a block at the target.
    Place,
}

impl EditAction {
    /// The wire value. Total, and that is the point of the missing variant.
    fn wire(self) -> fb::EditAction {
        match self {
            Self::Break => fb::EditAction::Break,
            Self::Place => fb::EditAction::Place,
        }
    }
}

/// One voxel the player is asking to change. **Intent, never outcome.**
///
/// The world's counterpart to [`PlayerInput`], and it carries exactly as little:
/// reach, line of sight, whether the target can be broken, whether the player holds
/// what they are placing — all of it is the server's to decide, and none of it is
/// stated here. There is no request id, because `schemas/world.fbs` is explicit that
/// there is no reply to correlate one with: a refused edit produces silence, and the
/// client must never read its own request as applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEditRequest {
    /// The voxel the player is aiming at, in world coordinates.
    pub pos: BlockCoord,
    /// Break or place.
    pub action: EditAction,
    /// The authoritative inventory slot to spend. Ignored by the server on a
    /// break, which remains legal until the mining issue withdraws it.
    pub slot: u8,
    /// The client's own tick counter, exactly as in [`PlayerInput`]: ordering and
    /// staleness only, never a clock.
    pub client_tick: u32,
}

/// What the server said about the voxel world.
///
/// The runs are carried **still encoded**, deliberately. Expanding them needs
/// `chunk_size`, which is a session parameter rather than a field of the frame,
/// and the invariants that depend on it belong where that number is known — see
/// `world::VoxelChunk::from_runs`, the mirror of `world.Decode` on the server. What
/// this module owes the rest of the client is that the values are copied out of the
/// peer's bytes, and they are.
///
/// A block edit is one of these rather than a kind of its own, because **ordering
/// across kinds is the property that matters**: an edit that overtook the
/// `ChunkData` it belongs to would be applied to a chunk that has since been
/// replaced wholesale by the generated one, and an edit that overtook a
/// `ChunkUnload` would resurrect bookkeeping for a chunk the session cannot see.
/// One queue cannot get that wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldUpdate {
    /// A chunk's voxels, run-length encoded as `(block id, run length)` pairs.
    Chunk { coord: ChunkCoord, runs: Vec<u16> },
    /// The chunk has left the session's view distance.
    Unload { coord: ChunkCoord },
    /// One voxel is now this block. Authoritative, and **not** an acknowledgement:
    /// the server sends it to every session that can see the voxel, so the only
    /// reading is "this voxel is now that block" and never "my edit succeeded".
    Block { pos: BlockCoord, block_id: u16 },
}

/// One entity's authoritative state, copied out of a snapshot.
///
/// Every float in one is finite, and that is a property of the type rather than a hope:
/// [`decode`] refuses a snapshot carrying anything else. `schemas/player.fbs` states the
/// invariant in this direction too — the server must never emit a non-finite position,
/// and the client validates rather than assuming it never does, because a NaN here would
/// pass unnoticed through the interpolation and into an entity's transform.
/// Whether the server considers this player alive or dead.
///
/// The wire enum's `Unknown = 0` is deliberately **not** a variant here, for the reason
/// [`EditAction`] has no `Unknown` either: it exists on the wire so that vitals carrying
/// no life state fail closed, and on this side it would only be a value every consumer
/// had to remember could not really happen. Making it unrepresentable is the stronger
/// promise, and the decoder is where it is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeState {
    Alive,
    Dead,
}

impl LifeState {
    /// `None` for `Unknown` and for any member a newer contract adds. Refusing an
    /// unknown life state is the point: guessing "alive" would draw a living player the
    /// server never said was living.
    fn from_wire(value: fb::LifeState) -> Option<Self> {
        match value {
            fb::LifeState::Alive => Some(Self::Alive),
            fb::LifeState::Dead => Some(Self::Dead),
            _ => None,
        }
    }
}

/// What kind of creature a [`MobState`] describes.
///
/// No `Unknown` variant, for the reason [`LifeState`] has none. An unknown kind is a
/// contract this build does not speak, and spawning a default enemy in its place would
/// put a creature in the world the server never said was there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobKind {
    Draugr,
    Vargr,
}

impl MobKind {
    /// **Accepting a member is a decision about the renderer, not about the decoder.**
    /// A kind listed here and drawn nowhere would spawn a body with no mesh; a kind the
    /// renderer can draw and this list refuses ends the session, because
    /// [`crate::net::session`] turns every decode error into a protocol failure. The two
    /// therefore move in the same commit, and the vargr arrived with the one that drew
    /// it — see [`crate::player::mobs`].
    fn from_wire(value: fb::MobKind) -> Option<Self> {
        match value {
            fb::MobKind::Draugr => Some(Self::Draugr),
            fb::MobKind::Vargr => Some(Self::Vargr),
            _ => None,
        }
    }
}

/// What a mob is doing this tick, as the server's state machine holds it.
///
/// Presentation reads this to telegraph an attack. It never advances it: the action
/// changes when a newer snapshot says so and at no other moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobAction {
    Idle,
    Chase,
    Windup,
    Recovery,
}

impl MobAction {
    fn from_wire(value: fb::MobAction) -> Option<Self> {
        match value {
            fb::MobAction::Idle => Some(Self::Idle),
            fb::MobAction::Chase => Some(Self::Chase),
            fb::MobAction::Windup => Some(Self::Windup),
            fb::MobAction::Recovery => Some(Self::Recovery),
            _ => None,
        }
    }
}

/// What kind of thing a [`StructureState`] describes.
///
/// No `Unknown` variant, for the reason [`MobKind`] has none. An unknown kind is a
/// contract this build does not speak, and drawing a default shelter in its place would
/// put a building in the world the server never said was there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureKind {
    Tent,
    Forge,
    Campfire,
}

impl StructureKind {
    /// The same rule [`MobKind::from_wire`] carries: a member is accepted here in the
    /// commit that teaches [`crate::player::structures`] to draw it, never before.
    fn from_wire(value: fb::StructureKind) -> Option<Self> {
        match value {
            fb::StructureKind::Tent => Some(Self::Tent),
            fb::StructureKind::Forge => Some(Self::Forge),
            fb::StructureKind::Campfire => Some(Self::Campfire),
            _ => None,
        }
    }
}

/// Which of the four ways a structure faces.
///
/// The compass `schemas/player.fbs` defines, and the one the movement basis already
/// uses: **North is -Z, East is +X, South is +Z, West is -X**, so a yaw of 0 — which
/// looks along -Z — is North. No `Unknown` variant here for the reason [`EditAction`]
/// has none: it exists on the wire so an omitted facing fails closed on the server, and
/// on this side it would only be a value every encoder had to promise never to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    North,
    East,
    South,
    West,
}

impl Facing {
    fn from_wire(value: fb::Facing) -> Option<Self> {
        match value {
            fb::Facing::North => Some(Self::North),
            fb::Facing::East => Some(Self::East),
            fb::Facing::South => Some(Self::South),
            fb::Facing::West => Some(Self::West),
            _ => None,
        }
    }

    /// The wire member. Total, because the `Unknown` the contract fails closed on is
    /// unrepresentable here.
    fn wire(self) -> fb::Facing {
        match self {
            Self::North => fb::Facing::North,
            Self::East => fb::Facing::East,
            Self::South => fb::Facing::South,
            Self::West => fb::Facing::West,
        }
    }
}

/// Which hair model a character wears.
///
/// **A variant for every member the contract declares, `Unknown` excepted** — and that
/// is a different rule from [`StructureKind`]'s, deliberately. A structure kind is
/// admitted here in the commit that teaches the client to *draw* it, because drawing a
/// default shelter would put a building in the world nobody placed. Hair is not a thing
/// in the world: it is a property of a character the player already chose, and refusing
/// one this build has no mesh for would refuse the whole player. So every declared
/// member decodes, and choosing a mesh for one is the renderer's problem rather than
/// the codec's.
///
/// `Unknown` has no variant, for the reason [`Facing`] has none: it exists on the wire
/// so an absent field fails closed, and making it unrepresentable here is the stronger
/// version of the promise that nothing ever renders one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HairModel {
    Shaved,
    Cropped,
    Braided,
    Loose,
    Topknot,
}

impl HairModel {
    /// Every model a character may wear, in the order the contract declares them.
    ///
    /// A hand-written list, because no stable Rust enumerates an enum's variants — but
    /// not an unpinned one, and that is the difference from `ItemShape::ALL`. What this
    /// list is *for* is the screen that offers a choice, so a model missing from it is a
    /// model no player can pick: not a build failure, because nothing here fails to
    /// compile, and a gap the compiler cannot see.
    ///
    /// **There is a contract underneath, so the list is derived from it rather than
    /// trusted.** `every_wire_hair_model_but_unknown_is_one_a_player_can_pick` walks
    /// `fb::HairModel::ENUM_VALUES` — flatc's own output, regenerated from
    /// `schemas/handshake.fbs` — and requires every declared member but `Unknown` to
    /// decode *and* to appear here. Appending a sixth model to the schema therefore
    /// fails a test rather than shipping a head nobody can choose.
    pub const ALL: [Self; 5] = [
        Self::Shaved,
        Self::Cropped,
        Self::Braided,
        Self::Loose,
        Self::Topknot,
    ];

    /// What a player reads on the control that chooses it. Display text, and the one
    /// place this client spells a hair model.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Shaved => "SHAVED",
            Self::Cropped => "CROPPED",
            Self::Braided => "BRAIDED",
            Self::Loose => "LOOSE",
            Self::Topknot => "TOPKNOT",
        }
    }

    fn from_wire(value: fb::HairModel) -> Option<Self> {
        match value {
            fb::HairModel::Shaved => Some(Self::Shaved),
            fb::HairModel::Cropped => Some(Self::Cropped),
            fb::HairModel::Braided => Some(Self::Braided),
            fb::HairModel::Loose => Some(Self::Loose),
            fb::HairModel::Topknot => Some(Self::Topknot),
            _ => None,
        }
    }

    /// The wire member. Total, because the `Unknown` the contract fails closed on is
    /// unrepresentable here.
    fn wire(self) -> fb::HairModel {
        match self {
            Self::Shaved => fb::HairModel::Shaved,
            Self::Cropped => fb::HairModel::Cropped,
            Self::Braided => fb::HairModel::Braided,
            Self::Loose => fb::HairModel::Loose,
            Self::Topknot => fb::HairModel::Topknot,
        }
    }
}

/// What a character looks like: four worn colours, a hair model and its colour.
///
/// **It cannot hold a value this contract forbids, and that is what changed with the
/// screen that builds one.** The fields were public while nothing outside this module
/// constructed an appearance; the doc then said so in as many words and named this issue
/// as the moment the promise would have to become a property. It is one now:
/// [`Appearance::new`] is the only way in from outside, it refuses a colour with bits
/// outside `0x00RRGGBB`, and the hair model cannot be `Unknown` because [`HairModel`] has
/// no such variant. So [`encode_create_character_request`] always writes a legal
/// appearance, and it needs no caveat to say so.
///
/// The client renders these and never substitutes a default of its own — the one
/// documented placeholder is [`PLACEHOLDER_APPEARANCE`], and it is for an appearance
/// that has not *arrived*, never for one that arrived wrong.
///
/// Each colour is `0x00RRGGBB`: eight bits per channel, sRGB, non-linear, top eight
/// bits zero. `schemas/common.fbs` is authoritative and there is no second encoding
/// anywhere on this wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    skin_color: u32,
    shirt_color: u32,
    trousers_color: u32,
    shoes_color: u32,
    hair_model: HairModel,
    hair_color: u32,
}

/// The one way an [`Appearance`] is refused: a colour outside `0x00RRGGBB`.
///
/// It names the field rather than only the value, because both readers need that. The
/// decoder turns it into [`DecodeError::AppearanceColorReserved`], which is a sentence an
/// operator reads about a *server*; the character screen cannot produce one at all, since
/// every colour it offers is a constant this build compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForbiddenColor {
    pub field: &'static str,
    pub value: u32,
}

impl Appearance {
    /// The bits a colour on this wire may use: `0x00RRGGBB`, with the top eight
    /// reserved and required to be zero.
    const COLOR_CHANNELS: u32 = 0x00FF_FFFF;

    /// Builds one, or names the colour that is not one this contract allows.
    ///
    /// **The single constructor, and the reason it is fallible is the reason it exists.**
    /// A `const fn`, so [`PLACEHOLDER_APPEARANCE`] goes through it and a palette entry
    /// could too; and total over the hair model, because the wire's `Unknown` has no
    /// variant here.
    ///
    /// **Absence is deliberately not a failure**, and it is not a question this
    /// constructor can even be asked: a table scalar equal to its default is not written
    /// at all, so an absent colour and a chosen black are the same bytes — refusing
    /// absence would refuse a character wearing black shoes, and would make decode
    /// correctness depend on the sender's builder settings. `schemas/common.fbs` carries
    /// the whole argument.
    pub const fn new(
        skin_color: u32,
        shirt_color: u32,
        trousers_color: u32,
        shoes_color: u32,
        hair_model: HairModel,
        hair_color: u32,
    ) -> Result<Self, ForbiddenColor> {
        // One check per colour, spelled once: a `const fn` cannot take a closure, and a
        // fifth colour added without a check is exactly what this shape prevents.
        let checked = [
            ("skin_color", skin_color),
            ("shirt_color", shirt_color),
            ("trousers_color", trousers_color),
            ("shoes_color", shoes_color),
            ("hair_color", hair_color),
        ];
        let mut i = 0;
        while i < checked.len() {
            let (field, value) = checked[i];
            if value & !Self::COLOR_CHANNELS != 0 {
                return Err(ForbiddenColor { field, value });
            }
            i += 1;
        }

        Ok(Self {
            skin_color,
            shirt_color,
            trousers_color,
            shoes_color,
            hair_model,
            hair_color,
        })
    }

    /// Skin, and the colour the hands take with it.
    pub const fn skin_color(self) -> u32 {
        self.skin_color
    }

    /// The shirt, tunic or coat covering the torso.
    pub const fn shirt_color(self) -> u32 {
        self.shirt_color
    }

    /// The trousers, breeches or leggings covering the legs.
    pub const fn trousers_color(self) -> u32 {
        self.trousers_color
    }

    /// Footwear.
    pub const fn shoes_color(self) -> u32 {
        self.shoes_color
    }

    /// Which hair model this character wears.
    pub const fn hair_model(self) -> HairModel {
        self.hair_model
    }

    /// The hair's colour, read whatever the model is: a shaved head still has stubble,
    /// and how much of the colour a model shows is the renderer's decision.
    pub const fn hair_color(self) -> u32 {
        self.hair_color
    }
}

/// One character an account owns on this world, as `ServerCharacterList` lists it.
///
/// Enough to draw a row in a character-select screen and nothing else: no position, no
/// health, no inventory. Those are the world's answer to "who is this", read once a
/// character has been chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterSummary {
    /// Server-minted and stable for the life of the character. **Not an entity id**:
    /// that names a body in a running simulation and is forgotten when the session
    /// ends, while this outlives every session the character has.
    pub character_id: u64,
    /// Display text. Shown, never parsed, and never an identifier.
    pub name: String,
    pub appearance: Appearance,
}

/// Every character this account owns on this world, and how many it may hold.
///
/// An empty `characters` is a legal, expected answer and not a refusal: it says the
/// only way forward is a `CreateCharacterRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterList {
    pub characters: Vec<CharacterSummary>,
    /// How many characters this account may hold here, including the ones above. Sent
    /// rather than hardcoded, for the reason every limit in `ServerWelcome` is: the
    /// number belongs to the server.
    pub max_characters: u8,
}

/// What one player entity looks like, sent once as that player comes into view.
///
/// Cached against the entity id and not resent. **Not part of a snapshot, and that is
/// the whole point**: `EntityState` is a struct inlined once per visible entity per
/// tick, and five colours that never change would be paid for at the tick rate for
/// ever. See `schemas/player.fbs`.
///
/// An appearance for an entity this client has never seen is **not** an error: the two
/// streams are not ordered against each other, so either can arrive first and a
/// receiver holds whichever half it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerAppearance {
    pub entity_id: u64,
    pub appearance: Appearance,
}

/// One character a client asks the server to create. **Intent only.**
///
/// The name is untrusted text the *server* judges — an unacceptable one is
/// `RejectReason::CHARACTER_NAME_REFUSED`, a refusal with a reply — so nothing here
/// checks it. The appearance is already validated, because it is an [`Appearance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCharacterRequest {
    pub name: String,
    pub appearance: Appearance,
}

/// Which recipe a [`CraftRequest`] names.
///
/// No `Unknown` variant, for the reason [`Facing`] has none: the wire carries one so an
/// omitted field fails closed on the *server*, and on this side it would only be a value
/// every encoder had to promise never to send. Making it unrepresentable is the stronger
/// version of that promise.
///
/// **The identity is all that crosses.** What a recipe costs, what it yields and where it
/// can be made are the server's table and are deliberately never sent — see
/// `schemas/player.fbs`. The client keeps a display-only mirror in
/// [`crate::player::RECIPES`] so it can gray out a row nobody can afford; a drift between
/// the two copies can show a wrong label but can never create an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeId {
    Forge,
    IronSword,
    SharpeningStone,
    Tent,
    Campfire,
    LeatherPatch,
    Shovel,
    Pickaxe,
    Axe,
}

impl RecipeId {
    /// The wire member. Total, because the `Unknown` the contract fails closed on is
    /// unrepresentable here.
    ///
    /// `pub(crate)` for one reader outside this module: the mirror's sweep in
    /// [`crate::player::RECIPES`]'s tests holds every row against `RecipeID::ENUM_VALUES`,
    /// and this is the mapping that makes the two comparable. Handing it out is what lets
    /// the contract itself be the source that sweep reads, rather than a second list of
    /// members kept beside it — which is the drift the sweep exists to catch.
    pub(crate) fn wire(self) -> fb::RecipeID {
        match self {
            Self::Forge => fb::RecipeID::Forge,
            Self::IronSword => fb::RecipeID::IronSword,
            Self::SharpeningStone => fb::RecipeID::SharpeningStone,
            Self::Tent => fb::RecipeID::Tent,
            Self::Campfire => fb::RecipeID::Campfire,
            Self::LeatherPatch => fb::RecipeID::LeatherPatch,
            Self::Shovel => fb::RecipeID::Shovel,
            Self::Pickaxe => fb::RecipeID::Pickaxe,
            Self::Axe => fb::RecipeID::Axe,
        }
    }
}

/// The recipient's own health and life state, from the newest snapshot.
///
/// Replaces the previous value wholesale, exactly as an [`InventoryState`] does. There
/// is nothing to merge and nothing to advance locally: a dropped snapshot is harmless
/// because the next one carries the complete answer.
///
/// Every invariant `schemas/player.fbs` documents has already been checked by the time
/// one of these exists — `max_health` is non-zero, so the ratio a health bar draws
/// cannot divide by zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerVitals {
    /// Current health. Zero is legal and means dead — but it is `life_state` that says
    /// so, not this number.
    pub health: u16,
    /// Maximum health. Guaranteed non-zero.
    pub max_health: u16,
    pub life_state: LifeState,
    /// Server ticks until the server respawns this player, at
    /// [`SessionParams::tick_rate`]. Zero unless `life_state` is [`LifeState::Dead`].
    /// **A count, never a deadline**: converted for display, held unchanged while
    /// snapshots are absent, never run down from local time.
    pub respawn_ticks: u32,
    /// Whether the server is currently refusing damage to this player. The server owns
    /// the timer; this is its answer.
    pub invulnerable: bool,
}

impl PlayerVitals {
    /// A living player at full health, for tests that need a valid snapshot without
    /// being about vitals.
    ///
    /// Test-only, deliberately: in production one of these only ever comes out of
    /// [`decode`], and a constructor a system could reach would be a place for the
    /// client to invent a health value the server never sent.
    #[cfg(test)]
    pub fn unharmed() -> Self {
        Self {
            health: 100,
            max_health: 100,
            life_state: LifeState::Alive,
            respawn_ticks: 0,
            invulnerable: false,
        }
    }
}

/// One mob's authoritative state in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MobState {
    /// Server-assigned identity, non-zero and shared with no player, drop or other mob
    /// in the same snapshot.
    pub entity_id: u64,
    pub kind: MobKind,
    pub pos: [f32; 3],
    /// Carried for diagnostics, exactly as [`EntityState::vel`] is: the client
    /// interpolates between two positions rather than extrapolating from a velocity.
    pub vel: [f32; 3],
    pub yaw: f32,
    pub health: u16,
    /// Guaranteed non-zero.
    pub max_health: u16,
    pub action: MobAction,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityState {
    /// Server-assigned identity. The local player's is `SessionParams::entity_id`.
    pub entity_id: u64,
    /// Where the server says this entity is, in blocks.
    pub pos: [f32; 3],
    /// How fast it is going, in blocks per second. Carried for diagnostics: the client
    /// interpolates between positions rather than extrapolating from a velocity.
    pub vel: [f32; 3],
    /// Facing, in radians, wrapped into (-π, π] by the server.
    pub yaw: f32,
}

/// One authoritative dropped item beside the player entities in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemDropState {
    pub entity_id: u64,
    pub pos: [f32; 3],
    pub item_id: u16,
    pub count: u16,
}

/// One placed structure's authoritative state in a snapshot.
///
/// **No position and no velocity, deliberately** — the contract carries neither. A
/// structure sits on the voxel grid and faces one of four ways, so an anchor cell and a
/// [`Facing`] say everything about where it is, and there is nothing here for an
/// interpolator to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructureState {
    /// Server-minted identity, non-zero and shared with no player, drop, mob or other
    /// structure in the same snapshot.
    pub structure_id: u64,
    pub kind: StructureKind,
    /// The voxel the structure rests **on**. The footprint above it follows from the
    /// kind and the facing, and both sides derive it the same way.
    pub anchor: BlockCoord,
    pub facing: Facing,
    /// The entity id the owner holds right now, or `0` while the owner has no live
    /// session — legal in this one field from protocol V5 on, and nowhere else. It is what
    /// lets a session tell its own camp from someone else's; it is not a permission this
    /// client enforces, because removal is refused by the server and not by the renderer.
    ///
    /// A session handle, not an identity: the server keys ownership by the identity behind
    /// `ClientHello.player_token`, which never crosses the wire. So `0` does not mean
    /// unowned, and a value that changes does not mean the structure changed hands.
    pub owner_entity_id: u64,
}

/// One tick of authoritative state for everything a session can see.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The server tick this describes. Monotonic per session, and a `u32` — which is why
    /// the client compares ticks with wrap-aware arithmetic.
    pub server_tick: u32,
    /// Every entity in view. Empty is legal and means exactly that.
    pub entities: Vec<EntityState>,
    /// Every dropped item in view. Separate from `entities` by contract.
    pub drops: Vec<ItemDropState>,
    /// Every mob in view. The newest snapshot is the **complete** set: a mob that stops
    /// appearing has stopped existing for this session, and the reason is never
    /// inferred from its health.
    pub mobs: Vec<MobState>,
    /// This client's own health and life state. Present in every snapshot by contract.
    pub self_vitals: PlayerVitals,
    /// Every structure in view, under the same complete-existence-set rule `mobs` obeys:
    /// one that stops appearing has stopped existing for this session. Removed by its
    /// owner, collapsed under a broken block and simply out of view are the same fact on
    /// the wire, and the client is not entitled to distinguish them.
    pub structures: Vec<StructureState>,
    /// Where this tick falls in the world's day, and zero from a server that keeps no
    /// clock.
    ///
    /// **Carried unvalidated by this layer, and deliberately so.** The bound it has to
    /// satisfy — less than `SessionParams::day_length_ticks` — names a number that
    /// arrived in a different message, and [`decode`] sees one frame at a time. The
    /// handshake owns that check, exactly as it owns the inventory's slot count, and
    /// for the same reason: it is the only layer that holds both halves.
    pub tick_of_day: u32,
}

#[cfg(test)]
impl Default for Snapshot {
    /// An empty snapshot of an unharmed living player, for the fixtures across this
    /// crate that need a valid one without being about any of it.
    ///
    /// Test-only, and `Snapshot` deliberately does not derive this: [`PlayerVitals`] has
    /// no meaningful default — a zero `max_health` is the absent-field case the decoder
    /// refuses — so a `Default` a system could reach would be a shape that never comes
    /// off the wire.
    fn default() -> Self {
        Self {
            server_tick: 0,
            entities: Vec::new(),
            drops: Vec::new(),
            mobs: Vec::new(),
            self_vitals: PlayerVitals::unharmed(),
            structures: Vec::new(),
            tick_of_day: 0,
        }
    }
}

/// One tick of intent, as the client sends it.
///
/// One tick of movement intent. **It carries no position**: the client says what the
/// player is trying to do and the server says what happened. See the header of
/// `schemas/player.fbs`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayerInput {
    /// This client's own tick counter. The server reads it as an order, never as a clock.
    pub client_tick: u32,
    /// Strafe intent, -1 (left) to 1 (right).
    pub move_x: f32,
    /// Forward intent, -1 (backward) to 1 (forward).
    pub move_z: f32,
    /// Facing, in radians.
    pub yaw: f32,
    /// Pitch, in radians. Carried for aiming; the server's movement ignores it.
    pub pitch: f32,
    /// Whether the jump control is held. Whether a jump *happens* is the server's call.
    pub jump: bool,
}

/// One slot in the inventory the server sent. The default is an empty slot.
///
/// Durability lives here rather than in a parallel `Vec` for the reason it does not on
/// the wire: `schemas/player.fbs`'s pair encoding is append-only and could not grow a
/// third scalar per slot, so durability rides in two more slot-indexed vectors. Off the
/// wire there is no such constraint, and one value per slot is one length instead of
/// three — so nothing downstream can pair slot `i`'s count with slot `j`'s durability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InventoryStack {
    pub item_id: u16,
    pub count: u16,
    /// Current durability. `0` under a non-zero `max_durability` is a worn-out item —
    /// unusable, still carried, still in its slot — and never an empty slot.
    pub durability: u16,
    /// Maximum durability, and the denominator of the ratio a durability bar draws.
    /// `0` means the slot's contents do not wear out at all: every resource, and every
    /// empty slot.
    pub max_durability: u16,
}

/// The player's complete authoritative inventory.
///
/// Every message replaces the previous value wholesale. There are no deltas and no
/// client-side count operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryState {
    pub stacks: Vec<InventoryStack>,
}

/// Starts, continues or cancels mining one voxel. Intent only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MineRequest {
    pub pos: BlockCoord,
    pub active: bool,
    pub client_tick: u32,
    /// The inventory slot this player is mining with.
    ///
    /// Which slot, never what is in it: the server reads its own inventory for the item.
    /// Appended in V8, and mining was the one action on this wire naming no slot — an
    /// attack, a placement and a repair all already did.
    pub slot: u8,
}

/// Authoritative progress for mining one voxel. `progress` is a fraction of 255.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MineProgress {
    pub pos: BlockCoord,
    pub progress: u8,
}

/// Which action a server refused, in an [`ActionRefused`].
///
/// **An `Unknown` variant, unlike [`Facing`] and [`RecipeId`], and the direction is
/// why.** Those two ride out of this client, so a value it can never construct is a
/// promise the type keeps for it. This one arrives, from a server that may be one
/// contract ahead — so the value it cannot name has to be representable, or the frame
/// would have to be refused, and refusing a frame ends a session. A refusal is the least
/// important message on this wire; ending a session over one would be the worst trade in
/// the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedAction {
    /// The contract's zero, or a member this build has no name for.
    Unknown,
    PlaceStructure,
    MineBlock,
    EditBlock,
    Craft,
    Repair,
}

impl RefusedAction {
    /// Total, and deliberately so: every value that is not a member this build knows is
    /// `Unknown`, which is what the contract's own zero means.
    fn from_wire(value: fb::RefusedAction) -> Self {
        match value {
            fb::RefusedAction::PlaceStructure => Self::PlaceStructure,
            fb::RefusedAction::MineBlock => Self::MineBlock,
            fb::RefusedAction::EditBlock => Self::EditBlock,
            fb::RefusedAction::Craft => Self::Craft,
            fb::RefusedAction::Repair => Self::Repair,
            _ => Self::Unknown,
        }
    }
}

/// Why a server refused an action, in an [`ActionRefused`].
///
/// **Not [`Reject`], which is why a *connection* was refused.** `schemas/handshake.fbs`
/// owns that one and spells it in codes because the player is shown it verbatim; these
/// are domain tokens and reach the screen only as sentences this client writes.
///
/// The two groups `schemas/player.fbs` splits the enum into survive as
/// [`Self::is_client_defect`]: a world that said no is news for the player, a request no
/// correct client sends is news for a log. `Unknown` is neither and is shown to nobody —
/// a sentence invented for a code this build cannot read would be a guess presented as
/// the server's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// The contract's zero, or a member this build has no name for.
    Unknown,

    // The world, or the player's own state, refused a request that was well formed.
    GroundNotGenerated,
    GroundIsAir,
    SpaceNotGenerated,
    SpaceBlocked,
    OutOfReach,
    PlayerIsDead,
    SlotEmpty,
    SlotUnusable,
    SlotChanged,
    InventoryBusy,
    TentAlreadyPlaced,

    // The request said something no correct client sends.
    MalformedNoAnchor,
    MalformedFacing,
    MalformedSlot,
    MalformedKind,
}

impl RefusalReason {
    /// Total, for the reason [`RefusedAction::from_wire`] is: an unreadable reason must
    /// cost the feedback and nothing else.
    fn from_wire(value: fb::RefusalReason) -> Self {
        match value {
            fb::RefusalReason::GroundNotGenerated => Self::GroundNotGenerated,
            fb::RefusalReason::GroundIsAir => Self::GroundIsAir,
            fb::RefusalReason::SpaceNotGenerated => Self::SpaceNotGenerated,
            fb::RefusalReason::SpaceBlocked => Self::SpaceBlocked,
            fb::RefusalReason::OutOfReach => Self::OutOfReach,
            fb::RefusalReason::PlayerIsDead => Self::PlayerIsDead,
            fb::RefusalReason::SlotEmpty => Self::SlotEmpty,
            fb::RefusalReason::SlotUnusable => Self::SlotUnusable,
            fb::RefusalReason::SlotChanged => Self::SlotChanged,
            fb::RefusalReason::InventoryBusy => Self::InventoryBusy,
            fb::RefusalReason::TentAlreadyPlaced => Self::TentAlreadyPlaced,
            fb::RefusalReason::MalformedNoAnchor => Self::MalformedNoAnchor,
            fb::RefusalReason::MalformedFacing => Self::MalformedFacing,
            fb::RefusalReason::MalformedSlot => Self::MalformedSlot,
            fb::RefusalReason::MalformedKind => Self::MalformedKind,
            _ => Self::Unknown,
        }
    }

    /// Whether this reason says the *request* was wrong rather than the world.
    ///
    /// A `match` rather than a comparison against the contract's boundary value, and the
    /// difference matters: an arithmetic test would classify a member appended in a
    /// contract this build has never read, which is a guess about a group it cannot see.
    /// Every arm here is a member this build knows, and everything else is `Unknown` —
    /// neither group, told to nobody.
    ///
    /// A correct client never produces one of these, so seeing one is a defect in *this*
    /// build. It goes to the log, where a defect belongs, and not to a player who did
    /// nothing wrong and can do nothing about it.
    pub fn is_client_defect(self) -> bool {
        matches!(
            self,
            Self::MalformedNoAnchor
                | Self::MalformedFacing
                | Self::MalformedSlot
                | Self::MalformedKind
        )
    }
}

/// The server's answer to an action it refused.
///
/// **The only message in this contract that says no to anything but a connection.** It
/// is not an acknowledgement and has no accepted half: a structure exists when a snapshot
/// says it does, and the absence of one of these is not a yes.
///
/// Nothing here is a decision. The client draws where a structure *would* stand and
/// repeats what the server said about it; the rule that produced the answer lives on the
/// server and only there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionRefused {
    pub action: RefusedAction,
    pub reason: RefusalReason,
    /// The voxel the refused request named, or `None` when it named none — which
    /// includes the request whose missing anchor was the refusal. Never a zero standing
    /// in for absent: the origin is a real place.
    pub anchor: Option<BlockCoord>,
}

/// One swing of the held weapon. Intent only, and it names no victim.
///
/// The server picks a target from the positions it owns and the aim it last accepted on
/// this client's `PlayerInput`, so there is nothing here for it to disbelieve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttackRequest {
    /// The authoritative inventory slot the swing spends.
    pub slot: u8,
    /// This client's own tick counter — the same one `PlayerInput` uses, so the server
    /// sees the aim frame carrying that tick before the swing that names it.
    pub client_tick: u32,
}

/// Intent to craft one recipe. It names no materials and no product.
///
/// Both are the server's, read from the authoritative inventory and the authoritative
/// world: a message that let the client state its own ingredients would be a cheat vector
/// however carefully the server re-checked them. Standing near the station a recipe needs
/// is the server's answer too, which is why a forge recipe is sent from anywhere and a
/// refusal is simply silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CraftRequest {
    /// Which recipe, and the whole of what this client gets to say.
    pub recipe: RecipeId,
    /// This client's own tick counter — the same one `PlayerInput` uses.
    pub client_tick: u32,
}

/// Intent to mend one carried item with one repair kit. Two slot indexes and nothing else.
///
/// There is no durability on this message in either direction, and that absence is the
/// whole of its safety: a client that could state a value would repair by asking. Which
/// slot holds a legal kit, whether the target wears out at all, how much wear one kit
/// gives back and whether there was room for it are the server's, read from the slots it
/// already owns — so both indexes travel verbatim and are refused by the simulation rather
/// than by the framing. A refusal is silence, and an accepted mend becomes visible only in
/// the durability vectors of the complete `InventoryState` that follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairRequest {
    /// The authoritative inventory slot holding the kit this mend spends.
    pub kit_slot: u8,
    /// The authoritative inventory slot holding the item to mend.
    pub target_slot: u8,
    /// This client's own tick counter — the same one `PlayerInput` uses.
    pub client_tick: u32,
}

/// Intent to plant one structure. It carries no kind and no id.
///
/// The kind follows from whatever the server's own inventory holds in `slot`, and the id
/// is minted when the structure comes into existence — a client that could name either
/// could name one it does not own. Reach, whether the footprint the facing implies is
/// clear and supported, and whether this player is already allowed another one are all
/// resolved server-side, and a refusal is silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceStructureRequest {
    /// The authoritative inventory slot the placement spends.
    pub slot: u8,
    /// The voxel the structure would rest on, in world block coordinates.
    pub anchor: BlockCoord,
    /// Which way it would face, quantized from the camera yaw by the client and
    /// validated by the server.
    pub facing: Facing,
    /// This client's own tick counter — the same one `PlayerInput` uses.
    pub client_tick: u32,
}

/// Intent to take one placed structure back.
///
/// It echoes an id the server minted and sent, which is the one kind of identifier a
/// client may hand back: ownership, reach and whether the structure still stands are all
/// re-read from the authoritative registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveStructureRequest {
    pub structure_id: u64,
    /// This client's own tick counter — the same one `PlayerInput` uses.
    pub client_tick: u32,
}

/// Intent to move up to `count` items between authoritative inventory slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryMoveRequest {
    pub from: u8,
    pub to: u8,
    pub count: u16,
}

/// A decoded `ServerReject`.
///
/// [`Self::describe`] is the one place the code and the detail become a single
/// string. It has one owner deliberately: `net/session.rs` produces that string and
/// `ui/status.rs` reads the code back out of it, so a separator chosen in two places
/// would let the two drift apart with every test still green.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject {
    /// The schema name of the reject code (`PROTOCOL_MISMATCH`, `SERVER_FULL`,
    /// `BAD_REQUEST`, `ALREADY_CONNECTED`). Codes are logged verbatim on both
    /// sides and shown to the player as-is, which is why the name is what gets
    /// carried. Reconnect policy is the one thing that would want to branch on
    /// the numeric code, and it arrives with its own issue.
    pub code: &'static str,
    /// Human-readable detail. Never parsed — branch on the code, display this.
    pub detail: String,
}

/// Separates the code from the detail in [`Reject::describe`]'s output, and is what
/// [`Reject::split_description`] looks for. One constant, so the two cannot disagree.
const REJECT_SEPARATOR: &str = ": ";

impl Reject {
    /// The refusal as one string: the code, and the server's detail when there is
    /// one.
    ///
    /// This is what reaches `ConnectionState::Rejected` and therefore the status
    /// line and the log. The code leads, because an operator greps for it.
    pub fn describe(&self) -> String {
        if self.detail.is_empty() {
            self.code.to_owned()
        } else {
            format!("{}{REJECT_SEPARATOR}{}", self.code, self.detail)
        }
    }

    /// Takes a string [`Self::describe`] produced back apart into code and detail.
    ///
    /// The inverse exists because the reject travels to the UI as display text
    /// rather than as this struct, and one refusal needs its code to be recognised
    /// there. Kept beside `describe` so the two share a separator instead of
    /// agreeing on one by coincidence — a test pins the round trip.
    ///
    /// Total over arbitrary strings: a refusal that was never a `ServerReject` (an
    /// unreachable address, a peer that is not speaking this protocol) simply has no
    /// code that matches anything, and the detail is `None`.
    pub fn split_description(description: &str) -> (&str, Option<&str>) {
        match description.split_once(REJECT_SEPARATOR) {
            Some((code, detail)) => (code, Some(detail)),
            None => (description, None),
        }
    }
}

/// One decoded envelope.
///
/// The variants are shaped by what the *client* does with a payload, not by the
/// union's membership, so several members share one variant and two of the
/// variants carry nothing but a name.
///
/// Which member maps to which variant is stated once per member in [`decode`] and
/// swept by `every_union_member_is_classified_deliberately`. No count of them is
/// kept here. This sentence used to say "three wire messages collapse into
/// `Message::Deferred`"; by Protocol V4 five did, and nothing said so, because a
/// number nobody can vary is a number nobody can see go wrong.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The session is accepted, and the parameters have already been validated.
    Welcome(SessionParams),
    /// The session is refused. The server closes the connection right after.
    Reject(Reject),
    /// Something happened to the voxel world the client is holding.
    World(WorldUpdate),
    /// One tick of authoritative entity state — where everything the session can see
    /// actually is.
    Snapshot(Snapshot),
    /// Everything this player carries, exactly as the server holds it.
    Inventory(InventoryState),
    /// Authoritative mining progress. No ECS system consumes it until the
    /// mining issue, but decoding and validation belong to Protocol V2 now.
    MineProgress(MineProgress),
    /// The server refused an action, and this is the reason a player reads.
    ///
    /// Named apart from [`Self::Reject`], which is the *connection* being refused and
    /// closes it. This one is an answer inside a session that goes on.
    ActionRefused(ActionRefused),
    /// Every character this account owns on this world — the second message of a V7
    /// handshake, and the one that asks which of them is playing.
    CharacterList(CharacterList),
    /// What one visible player looks like. Decoded and validated here; no ECS system
    /// consumes it until the appearance-rendering issue, exactly as `MineProgress` was
    /// decoded from V2 and drawn later.
    PlayerAppearance(PlayerAppearance),
    /// A server→client payload no system consumes yet, or a member added by a newer
    /// contract. Named for diagnostics; each becomes real in its own issue.
    Deferred(&'static str),
    /// A payload only a *client* sends. Direction is a protocol rule rather than
    /// a type rule — both sides share one union — so receiving one of these is a
    /// protocol error, the exact mirror of the server refusing a `ServerWelcome`
    /// from a client.
    ClientOnly(&'static str),
}

/// Why a frame is not a usable message.
///
/// Split finely enough that the failing invariant is named in the status text
/// and the log, because "malformed" on its own tells an operator nothing about
/// which side to fix.
///
/// `PartialEq` without `Eq`, because [`Self::NonFiniteSpawn`] reports the value
/// it refused and `f32` has no total equality — which is the very property that
/// made the finiteness check necessary in the first place.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    /// Too small to hold a root offset and the file identifier, so there is
    /// nothing that can be safely inspected at all.
    TooShort { len: usize },
    /// The four-byte tag says this is not a Voxelheim message.
    NotVoxelheim,
    /// The verifier refused the buffer: an offset, vtable or string in it does
    /// not point where it claims.
    Malformed(String),
    /// The union tag names a payload the envelope does not actually carry.
    MissingPayload(&'static str),
    /// `ServerWelcome.spawn` is absent. The server always sends it.
    MissingSpawn,
    /// A `spawn` component is NaN or infinite.
    NonFiniteSpawn { axis: usize, value: f32 },
    /// `tick_rate` violates `>= 1`.
    TickRate(u8),
    /// `chunk_size` violates `1..=MAX_CHUNK_SIZE`.
    ChunkSize(u16),
    /// `view_distance` violates `<= MAX_VIEW_DISTANCE`.
    ViewDistance(u8),
    /// `inventory_slots` must be non-zero.
    InventorySlots(u8),
    /// `hotbar_slots` must be non-zero.
    HotbarSlots(u8),
    /// Every hotbar slot must also be an inventory slot.
    HotbarExceedsInventory { hotbar: u8, inventory: u8 },
    /// `ServerWelcome.entity_id` is the reserved id 0.
    ///
    /// Zero is not an entity anywhere in this contract — a mob carrying it is
    /// refused, and so is a player. It matters here for a second reason: from V5 a
    /// structure may carry owner 0 to say its owner is offline, and
    /// `player/structures.rs` decides whose camp is whose by comparing that field
    /// against this one. A session whose own id were 0 would therefore claim every
    /// offline owner's camp as its own.
    WelcomeEntityId,
    /// `ServerWelcome.player_token` is absent. Every accepted handshake carries
    /// one, so a welcome without it is a server that has not settled an identity.
    MissingPlayerToken,
    /// The world clock's boundaries do not satisfy
    /// `0 < night_start_ticks < night_end_ticks <= day_length_ticks`.
    ///
    /// Only ever raised for a **declared** clock: a `day_length_ticks` of zero says
    /// this server keeps none, and the two boundaries are then not read at all.
    ///
    /// Refused rather than clamped, for the reason a NaN spawn is refused rather than
    /// bounded: a night that ends before it begins has no reading that is safe to
    /// guess at, and every repair a decoder could invent would be a different night
    /// from the one the server is simulating.
    WorldClock {
        day_length: u32,
        night_start: u32,
        night_end: u32,
    },
    /// `player_token` is present but is not [`PLAYER_TOKEN_LEN`] bytes — the empty
    /// vector included.
    ///
    /// The length is reported and the bytes are not, which is the whole of what a
    /// decoder is allowed to say about a bearer credential.
    PlayerTokenLength(usize),
    /// A chunk message carries no `coord`. Both `ChunkData` and `ChunkUnload` are
    /// meaningless without one — there is no "current chunk" to fall back to.
    MissingCoord(&'static str),
    /// A `BlockUpdate` carries no `pos`. Refused rather than read as the origin,
    /// which `schemas/world.fbs` spells out: the origin is a real location, so
    /// defaulting would edit the world at a point nobody named.
    MissingBlockPos(&'static str),
    /// A `MineProgress` carries no `pos`; the origin is never a default.
    MissingMinePos,
    /// `ChunkData` carries no `runs` at all, which is not the same as a chunk full
    /// of air: air is a run like any other.
    MissingRuns,
    /// A snapshot carries an entity with a NaN or infinite component.
    ///
    /// Refused rather than dropped, unlike a malformed *chunk*: a chunk is one hole in
    /// the terrain, while an entity whose position is not a number is a server that has
    /// lost track of the world it is authoritative over. `field` names the component so
    /// the log says which half went wrong.
    NonFiniteEntity {
        entity_id: u64,
        field: &'static str,
        value: f32,
    },
    /// A snapshot carries a drop with a NaN or infinite position component.
    NonFiniteDrop {
        entity_id: u64,
        field: &'static str,
        value: f32,
    },
    /// Item id 0 is reserved for no item and cannot name a drop.
    DropWithoutItem(u64),
    /// A drop with no items is despawned, never sent.
    EmptyDrop(u64),
    /// One id cannot name both a player and a drop in the same snapshot.
    PlayerDropEntityConflict(u64),
    /// The inventory vector is not complete `(item id, count)` pairs.
    InventoryLength(usize),
    /// A slot must be either `(0, 0)` or a non-zero item with a non-zero count.
    InventorySlotPair {
        slot: usize,
        item_id: u16,
        count: u16,
    },
    /// The three inventory vectors do not describe the same slots. Refused rather than
    /// padded: a short durability vector would silently report every slot past its end
    /// as indestructible.
    DurabilityLength {
        slots: usize,
        durability: usize,
        max_durability: usize,
    },
    /// A slot's durability is impossible against what the slot holds — a current value
    /// with no maximum, a current value above the maximum, or a durable item that is
    /// absent or stacked. A durable item is one whole item, always.
    SlotDurability {
        slot: usize,
        item_id: u16,
        count: u16,
        durability: u16,
        max_durability: u16,
    },
    /// `self_vitals.life_state` is `Unknown` — the absent-field case — or a member this
    /// build does not know. Never guessed at: drawing a living player the server did not
    /// say was living is the one mistake a health HUD must not make.
    UnknownLifeState,
    /// `max_health` is zero, or `health` exceeds it. The first is the division a health
    /// bar performs, and an honestly buggy server reaches it as easily as a hostile one.
    VitalsHealth { health: u16, max_health: u16 },
    /// An `Alive` player with no health left. Zero health is what the server's own
    /// transition to `Dead` means, so this is a server that has lost track of one of its
    /// own players.
    AliveWithoutHealth,
    /// Only a dead player counts down to a respawn.
    RespawnWhileAlive { respawn_ticks: u32 },
    /// A mob carries the reserved identity 0.
    MobWithoutIdentity,
    /// One id names a mob and a player, a mob and a drop, or two mobs, in one snapshot.
    MobEntityConflict(u64),
    /// A mob's `pos` or `vel` struct is absent. Refused rather than read as the origin,
    /// for the reason a `BlockUpdate` without a position is: the origin is a real place.
    MissingMobTransform { entity_id: u64, field: &'static str },
    /// A mob carries a NaN or infinite component, which would pass through
    /// interpolation into a `Transform` and never leave.
    NonFiniteMob {
        entity_id: u64,
        field: &'static str,
        value: f32,
    },
    /// A mob's `kind` or `action` is `Unknown`, or a member this build has no name for.
    /// Refused rather than defaulted: a default enemy is a creature the server never
    /// said was there.
    UnknownMobEnum {
        entity_id: u64,
        field: &'static str,
        value: u8,
    },
    /// A mob's `max_health` is zero, or its `health` exceeds it.
    MobHealth {
        entity_id: u64,
        health: u16,
        max_health: u16,
    },
    /// A structure carries the reserved identity 0.
    StructureWithoutIdentity,
    /// One id names a structure and a player, a drop, a mob, or another structure, in one
    /// snapshot. The schema's "globally unique" is a claim about the whole snapshot.
    StructureEntityConflict(u64),
    /// A structure's `anchor` struct is absent. Refused rather than read as the origin,
    /// for the reason a `BlockUpdate` without a position is: the origin is a real place.
    MissingStructureAnchor(u64),
    /// A structure's `kind` or `facing` is `Unknown`, or a member this build has no name
    /// for. Refused rather than defaulted: a default shelter is a building the server
    /// never said was there, and a default facing points its door somewhere nobody chose.
    UnknownStructureEnum {
        structure_id: u64,
        field: &'static str,
        value: u8,
    },
    /// A message that must describe a face carries no `appearance` table at all.
    ///
    /// Refused rather than filled in with [`PLACEHOLDER_APPEARANCE`]: the placeholder
    /// answers "the message has not arrived yet", and this message *did* arrive. `where`
    /// names the payload so the log says which half of the contract to look at.
    MissingAppearance { at: &'static str },
    /// A colour's reserved top eight bits are not zero.
    ///
    /// Refused rather than masked. A set high byte means the peer is encoding something
    /// this build does not know about, and masking it would draw a colour nobody chose
    /// while hiding the disagreement — the reasoning [`Self::WorldClock`] records, where
    /// a repair a decoder invents is a different answer from the server's.
    AppearanceColorReserved { field: &'static str, value: u32 },
    /// An `Appearance` carries `HairModel::Unknown` — the absent-field case — or a
    /// member this build has no name for. Never guessed at: `schemas/common.fbs` says
    /// the client renders what the player chose and invents no default.
    UnknownHairModel(u8),
    /// A `CharacterSummary` carries the reserved id 0, which names no character.
    CharacterWithoutIdentity,
    /// A `CharacterSummary` carries no name, or an empty one. A character with no name
    /// is a store that has lost one, not a character.
    CharacterWithoutName(u64),
    /// One `character_id` names two rows of the same list.
    DuplicateCharacter(u64),
    /// A `ServerCharacterList` says it allows no characters at all, or fewer than it
    /// just listed. A server disagreeing with itself about its own limit; taking the
    /// larger of the two would be inventing a limit nobody set.
    CharacterLimit { listed: usize, max: u8 },
    /// A `PlayerAppearance` carries the reserved entity id 0.
    ///
    /// Distinct from having no matching entity, which is **not** an error: the two
    /// streams are not ordered against each other. Zero names nobody at all.
    AppearanceWithoutEntity,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len } => write!(f, "{len} bytes cannot hold an envelope"),
            Self::NotVoxelheim => {
                write!(f, "not a {:?} buffer", fb::ENVELOPE_IDENTIFIER)
            }
            Self::Malformed(detail) => write!(f, "malformed envelope: {detail}"),
            Self::MissingPayload(kind) => write!(f, "{kind} payload is absent"),
            Self::MissingSpawn => write!(f, "server welcome carries no spawn"),
            Self::NonFiniteSpawn { axis, value } => {
                write!(f, "spawn axis {axis} must be finite, got {value}")
            }
            Self::TickRate(got) => write!(f, "tick rate must be at least 1, got {got}"),
            Self::ChunkSize(got) => {
                write!(f, "chunk size must be in 1..={MAX_CHUNK_SIZE}, got {got}")
            }
            Self::ViewDistance(got) => {
                write!(
                    f,
                    "view distance must be at most {MAX_VIEW_DISTANCE}, got {got}"
                )
            }
            Self::InventorySlots(got) => {
                write!(f, "inventory slot count must be non-zero, got {got}")
            }
            Self::HotbarSlots(got) => {
                write!(f, "hotbar slot count must be non-zero, got {got}")
            }
            Self::HotbarExceedsInventory { hotbar, inventory } => write!(
                f,
                "hotbar has {hotbar} slots, more than the inventory's {inventory}"
            ),
            Self::WelcomeEntityId => {
                write!(f, "server welcome carries the reserved entity id 0")
            }
            Self::WorldClock {
                day_length,
                night_start,
                night_end,
            } => write!(
                f,
                "world clock is out of order: night {night_start}..{night_end} in a day of {day_length} ticks"
            ),
            Self::MissingPlayerToken => write!(f, "server welcome carries no identity token"),
            // The length, never the bytes.
            Self::PlayerTokenLength(len) => write!(
                f,
                "identity token is {len} bytes, want exactly {PLAYER_TOKEN_LEN}"
            ),
            Self::MissingCoord(kind) => write!(f, "{kind} carries no chunk coordinate"),
            Self::MissingBlockPos(kind) => write!(f, "{kind} carries no block position"),
            Self::MissingMinePos => write!(f, "MineProgress carries no block position"),
            Self::MissingRuns => write!(f, "chunk data carries no runs"),
            Self::NonFiniteEntity {
                entity_id,
                field,
                value,
            } => write!(f, "entity {entity_id} has a non-finite {field}: {value}"),
            Self::NonFiniteDrop {
                entity_id,
                field,
                value,
            } => write!(f, "drop {entity_id} has a non-finite {field}: {value}"),
            Self::DropWithoutItem(entity_id) => {
                write!(f, "drop {entity_id} carries reserved item id 0")
            }
            Self::EmptyDrop(entity_id) => write!(f, "drop {entity_id} has count zero"),
            Self::PlayerDropEntityConflict(entity_id) => {
                write!(f, "entity id {entity_id} names both a player and a drop")
            }
            Self::InventoryLength(len) => {
                write!(f, "inventory has {len} values, want complete pairs")
            }
            Self::InventorySlotPair {
                slot,
                item_id,
                count,
            } => write!(
                f,
                "inventory slot {slot} is ({item_id}, {count}), want both zero or both non-zero"
            ),
            Self::DurabilityLength {
                slots,
                durability,
                max_durability,
            } => write!(
                f,
                "inventory has {slots} slots but {durability} durability and {max_durability} maximum entries"
            ),
            Self::SlotDurability {
                slot,
                item_id,
                count,
                durability,
                max_durability,
            } => write!(
                f,
                "inventory slot {slot} holds ({item_id}, {count}) with durability {durability}/{max_durability}, which is not a possible slot"
            ),
            Self::UnknownLifeState => write!(f, "vitals carry no known life state"),
            Self::VitalsHealth { health, max_health } => write!(
                f,
                "vitals are {health}/{max_health}, want a non-zero maximum and no more health than it"
            ),
            Self::AliveWithoutHealth => write!(f, "vitals say alive with no health left"),
            Self::RespawnWhileAlive { respawn_ticks } => write!(
                f,
                "vitals count {respawn_ticks} ticks to a respawn for a player who is not dead"
            ),
            Self::MobWithoutIdentity => write!(f, "a mob carries the reserved entity id 0"),
            Self::MobEntityConflict(entity_id) => write!(
                f,
                "entity id {entity_id} names a mob and something else in one snapshot"
            ),
            Self::MissingMobTransform { entity_id, field } => {
                write!(f, "mob {entity_id} carries no {field}")
            }
            Self::NonFiniteMob {
                entity_id,
                field,
                value,
            } => write!(f, "mob {entity_id} has a non-finite {field}: {value}"),
            Self::UnknownMobEnum {
                entity_id,
                field,
                value,
            } => write!(f, "mob {entity_id} has an unknown {field}: {value}"),
            Self::MobHealth {
                entity_id,
                health,
                max_health,
            } => write!(
                f,
                "mob {entity_id} is {health}/{max_health}, want a non-zero maximum and no more health than it"
            ),
            Self::StructureWithoutIdentity => {
                write!(f, "a structure carries the reserved structure id 0")
            }
            Self::StructureEntityConflict(structure_id) => write!(
                f,
                "id {structure_id} names a structure and something else in one snapshot"
            ),
            Self::MissingStructureAnchor(structure_id) => {
                write!(f, "structure {structure_id} carries no anchor")
            }
            Self::UnknownStructureEnum {
                structure_id,
                field,
                value,
            } => write!(
                f,
                "structure {structure_id} has an unknown {field}: {value}"
            ),
            Self::MissingAppearance { at } => write!(f, "{at} carries no appearance"),
            Self::AppearanceColorReserved { field, value } => write!(
                f,
                "appearance {field} is {value:#010x}; the top eight bits are reserved and must be zero"
            ),
            Self::UnknownHairModel(value) => {
                write!(f, "appearance has an unknown hair model: {value}")
            }
            Self::CharacterWithoutIdentity => {
                write!(f, "a character summary carries the reserved id 0")
            }
            Self::CharacterWithoutName(character_id) => {
                write!(f, "character {character_id} has no name")
            }
            Self::DuplicateCharacter(character_id) => {
                write!(f, "character {character_id} is listed twice")
            }
            Self::CharacterLimit { listed, max } => write!(
                f,
                "the list holds {listed} characters and says the limit is {max}"
            ),
            Self::AppearanceWithoutEntity => {
                write!(f, "a PlayerAppearance carries the reserved entity id 0")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Reads one frame into a [`Message`].
///
/// Total over arbitrary bytes: every path returns, none panics. That is the
/// requirement, not a nicety — this is the function a hostile peer gets to
/// choose the input of.
pub fn decode(frame: &[u8]) -> Result<Message, DecodeError> {
    // A root offset plus the identifier is the smallest thing that can be
    // inspected at all; below that there is not even a tag to check.
    let minimum = size_of::<flatbuffers::UOffsetT>() + fb::ENVELOPE_IDENTIFIER.len();
    if frame.len() < minimum {
        return Err(DecodeError::TooShort { len: frame.len() });
    }
    if !fb::envelope_buffer_has_identifier(frame) {
        return Err(DecodeError::NotVoxelheim);
    }

    // The verifying root, never `root_as_envelope_unchecked`. `root_as_envelope`
    // does not check the identifier, which is why the tag is tested above rather
    // than instead.
    let envelope =
        fb::root_as_envelope(frame).map_err(|err| DecodeError::Malformed(err.to_string()))?;

    let kind = envelope.payload_type();
    let name = kind.variant_name().unwrap_or(UNKNOWN_VARIANT);

    match kind {
        fb::Payload::ServerWelcome => {
            let welcome = envelope
                .payload_as_server_welcome()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::Welcome(session_params(&welcome)?))
        }
        fb::Payload::ServerReject => {
            let reject = envelope
                .payload_as_server_reject()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::Reject(Reject {
                code: reject.reason().variant_name().unwrap_or(UNKNOWN_VARIANT),
                // Untrusted display text: copied out, never parsed.
                detail: reject.detail().unwrap_or_default().to_owned(),
            }))
        }
        fb::Payload::ChunkData => {
            let chunk = envelope
                .payload_as_chunk_data()
                .ok_or(DecodeError::MissingPayload(name))?;
            let coord = chunk_coord(chunk.coord(), name)?;
            // Copied out, never borrowed: the accessor is a view into the frame,
            // and the frame is gone by the time the ECS looks. `iter().collect()`
            // rather than `bytes()`, because the vector is little-endian on the
            // wire and this client is not obliged to be.
            let runs = chunk.runs().ok_or(DecodeError::MissingRuns)?;
            Ok(Message::World(WorldUpdate::Chunk {
                coord,
                runs: runs.iter().collect(),
            }))
        }
        fb::Payload::ChunkUnload => {
            let unload = envelope
                .payload_as_chunk_unload()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::World(WorldUpdate::Unload {
                coord: chunk_coord(unload.coord(), name)?,
            }))
        }
        fb::Payload::BlockUpdate => {
            let update = envelope
                .payload_as_block_update()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::World(WorldUpdate::Block {
                pos: block_coord(update.pos(), name)?,
                // Carried verbatim, including an id this build has no colour for: the
                // palette lookup is bounds-checked and falls back to a placeholder,
                // and dropping the update instead would leave the voxel showing
                // whatever it used to be — a hole where the server says there is a
                // wall. See `world::palette`.
                block_id: update.block_id(),
            }))
        }
        fb::Payload::EntitySnapshot => {
            let snapshot = envelope
                .payload_as_entity_snapshot()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::Snapshot(entity_snapshot(&snapshot)?))
        }
        fb::Payload::InventoryState => {
            let inventory = envelope
                .payload_as_inventory_state()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::Inventory(inventory_state(&inventory)?))
        }
        fb::Payload::MineProgress => {
            let progress = envelope
                .payload_as_mine_progress()
                .ok_or(DecodeError::MissingPayload(name))?;
            let pos = progress.pos().ok_or(DecodeError::MissingMinePos)?;
            Ok(Message::MineProgress(MineProgress {
                pos: BlockCoord {
                    x: pos.x(),
                    y: pos.y(),
                    z: pos.z(),
                },
                progress: progress.progress(),
            }))
        }
        fb::Payload::ActionRefused => {
            let refused = envelope
                .payload_as_action_refused()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::ActionRefused(ActionRefused {
                // Both read through their `Unknown`, never refused. A reason this build
                // cannot name costs the sentence and nothing else; a `DecodeError` here
                // would end the session over the least important frame on the wire.
                action: RefusedAction::from_wire(refused.action()),
                reason: RefusalReason::from_wire(refused.reason()),
                // Absent is a real answer here rather than a missing field: the refused
                // request named no voxel, or named no anchor at all — which is itself one
                // of the refusals. Reading it as the origin would put the answer at (0,
                // 0, 0), where nobody was looking.
                anchor: refused.anchor().map(|anchor| BlockCoord {
                    x: anchor.x(),
                    y: anchor.y(),
                    z: anchor.z(),
                }),
            }))
        }
        fb::Payload::ServerCharacterList => {
            let list = envelope
                .payload_as_server_character_list()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::CharacterList(character_list(&list)?))
        }
        fb::Payload::PlayerAppearance => {
            let payload = envelope
                .payload_as_player_appearance()
                .ok_or(DecodeError::MissingPayload(name))?;
            let entity_id = payload.entity_id();
            if entity_id == 0 {
                return Err(DecodeError::AppearanceWithoutEntity);
            }
            // Nothing here asks whether the client knows this entity, and nothing may:
            // `schemas/player.fbs` is explicit that the appearance stream and the
            // snapshot stream are not ordered against each other, so an appearance for
            // an entity nobody has seen yet is the ordinary case rather than an error.
            Ok(Message::PlayerAppearance(PlayerAppearance {
                entity_id,
                appearance: appearance(payload.appearance(), "PlayerAppearance")?,
            }))
        }
        // Every payload only a *client* sends, which is the whole client→server half
        // of `schemas/envelope.fbs` rather than the part of it that existed when this
        // list was written. Four members added after `AttackRequest` inherited
        // `Deferred` from the fallback below — silently, because a fallback is an
        // answer for every member somebody forgets, and the answer it gave named them
        // as server→client payloads nothing consumes yet (legacy PR 131).
        fb::Payload::ClientHello
        | fb::Payload::PlayerInput
        | fb::Payload::BlockEditRequest
        | fb::Payload::ChunkResendRequest
        | fb::Payload::MineRequest
        | fb::Payload::InventoryMoveRequest
        | fb::Payload::AttackRequest
        | fb::Payload::CraftRequest
        | fb::Payload::RepairRequest
        | fb::Payload::PlaceStructureRequest
        | fb::Payload::RemoveStructureRequest
        | fb::Payload::SelectCharacterRequest
        | fb::Payload::CreateCharacterRequest => Ok(Message::ClientOnly(name)),
        // An envelope with no payload is not a message this client can act on, and the
        // handshake refuses it. Named rather than left to the fallback, so that the
        // fallback is reachable for nothing this build can put a name to.
        fb::Payload::NONE => Ok(Message::Deferred(name)),
        // A tag from a contract newer than this build. The arm cannot be deleted and
        // the compiler will never ask for a twentieth: flatc emits `Payload` as a
        // newtype over `u8` with associated constants, not as a Rust enum, so no match
        // over it is ever exhaustive. `every_union_member_is_classified_deliberately`
        // is what asks instead.
        //
        // It reports `UNKNOWN_VARIANT` rather than `name`, and the difference is the
        // point: for every tag that legitimately arrives here the two are the same
        // string, because `variant_name` returned `None` — that is what unknown means.
        // They differ only for a known member that leaked in, which is what makes this
        // arm distinguishable from the ones above at runtime, and so testable.
        _ => Ok(Message::Deferred(UNKNOWN_VARIANT)),
    }
}

/// Copies one appearance out of a frame and enforces every invariant
/// `schemas/common.fbs` attaches to it.
///
/// An `Option` because `appearance` is a nested table, so absence has to be answered.
/// It is refused rather than filled in with [`PLACEHOLDER_APPEARANCE`], for the reason
/// a `BlockUpdate` with no position is refused rather than read as the origin: the
/// placeholder means "the message has not arrived", and this one arrived.
fn appearance(
    value: Option<fb::Appearance<'_>>,
    at: &'static str,
) -> Result<Appearance, DecodeError> {
    let value = value.ok_or(DecodeError::MissingAppearance { at })?;

    // The hair model first, because it is the one field the constructor cannot judge:
    // `Unknown` is the wire's fail-closed member and this side has no variant for it, so
    // an appearance carrying one never becomes a value at all.
    let hair_model = HairModel::from_wire(value.hair_model())
        .ok_or(DecodeError::UnknownHairModel(value.hair_model().0))?;

    // And the colours through the one constructor, rather than a second copy of its
    // rule. Presence is deliberately not checked, and `schemas/common.fbs` carries the
    // reasoning: a table scalar equal to its default is not written at all, and black is
    // a legal colour — so an absent `skin_color` and a chosen `0x000000` are the same
    // bytes, and refusing absence would refuse black shoes.
    Appearance::new(
        value.skin_color(),
        value.shirt_color(),
        value.trousers_color(),
        value.shoes_color(),
        hair_model,
        value.hair_color(),
    )
    .map_err(|refused| DecodeError::AppearanceColorReserved {
        field: refused.field,
        value: refused.value,
    })
}

/// Copies a character list out of a frame and enforces what `schemas/handshake.fbs`
/// attaches to it.
///
/// An absent `characters` vector reads as an empty one — the two say the same thing,
/// "no characters here", which is a legal answer and not a refusal.
fn character_list(list: &fb::ServerCharacterList<'_>) -> Result<CharacterList, DecodeError> {
    let mut characters = Vec::new();
    let mut seen = HashSet::new();

    for summary in list.characters().into_iter().flatten() {
        let character_id = summary.character_id();
        if character_id == 0 {
            return Err(DecodeError::CharacterWithoutIdentity);
        }
        if !seen.insert(character_id) {
            return Err(DecodeError::DuplicateCharacter(character_id));
        }
        let name = summary.name().unwrap_or_default();
        if name.is_empty() {
            return Err(DecodeError::CharacterWithoutName(character_id));
        }
        characters.push(CharacterSummary {
            character_id,
            // Copied out, never borrowed: the accessor is a view into the frame, and
            // the frame is gone by the time anything draws a row from this.
            name: name.to_owned(),
            appearance: appearance(summary.appearance(), "CharacterSummary")?,
        });
    }

    // A server that allows no characters, or fewer than it has just listed, is
    // disagreeing with itself. Refused rather than repaired: taking the larger of the
    // two would invent a limit nobody set, and a client that offered a creation the
    // server will refuse is worse than one that says the frame was wrong.
    let max = list.max_characters();
    if max == 0 || usize::from(max) < characters.len() {
        return Err(DecodeError::CharacterLimit {
            listed: characters.len(),
            max,
        });
    }

    Ok(CharacterList {
        characters,
        max_characters: max,
    })
}

/// Copies and validates the three slot-indexed inventory vectors into one row per slot.
///
/// An absent vector reads as an empty one, which the length check below then refuses
/// against the others — the same treatment `stacks` already gets, and the reason a
/// server that emitted durability only when something was durable would produce a frame
/// nobody could read.
fn inventory_state(state: &fb::InventoryState<'_>) -> Result<InventoryState, DecodeError> {
    let values: Vec<u16> = state
        .stacks()
        .map(|stacks| stacks.iter().collect())
        .unwrap_or_default();
    if !values.len().is_multiple_of(2) {
        return Err(DecodeError::InventoryLength(values.len()));
    }
    let slots = values.len() / 2;

    let durability: Vec<u16> = state
        .durability()
        .map(|values| values.iter().collect())
        .unwrap_or_default();
    let max_durability: Vec<u16> = state
        .max_durability()
        .map(|values| values.iter().collect())
        .unwrap_or_default();
    // Checked before either is indexed, and against `stacks` rather than against each
    // other: a short durability vector padded to length would report every slot past its
    // end as indestructible, which is a lie the UI has no way to notice.
    if durability.len() != slots || max_durability.len() != slots {
        return Err(DecodeError::DurabilityLength {
            slots,
            durability: durability.len(),
            max_durability: max_durability.len(),
        });
    }

    let mut stacks = Vec::with_capacity(slots);
    for (slot, pair) in values.chunks_exact(2).enumerate() {
        let [item_id, count] = [pair[0], pair[1]];
        if (item_id == 0) != (count == 0) {
            return Err(DecodeError::InventorySlotPair {
                slot,
                item_id,
                count,
            });
        }

        let stack = InventoryStack {
            item_id,
            count,
            durability: durability[slot],
            max_durability: max_durability[slot],
        };
        // `(0, 0)` is a slot that does not wear out — empty, or a resource. Anything
        // else has to be one whole durable item: a maximum to divide by, a current value
        // that does not exceed it, and exactly one of the thing. A current value with no
        // maximum is the case worth naming, because it is what a partially-written
        // encoder produces.
        let impossible = if stack.max_durability == 0 {
            stack.durability != 0
        } else {
            stack.durability > stack.max_durability || item_id == 0 || count != 1
        };
        if impossible {
            return Err(DecodeError::SlotDurability {
                slot,
                item_id,
                count,
                durability: stack.durability,
                max_durability: stack.max_durability,
            });
        }
        stacks.push(stack);
    }
    Ok(InventoryState { stacks })
}

/// Copies a `ChunkCoord` struct out of the buffer.
///
/// A struct field is optional in FlatBuffers like any other, so absence has to be
/// answered. It is refused rather than defaulted to the origin: a chunk with no
/// address would be stored, meshed and drawn at 0,0,0 on top of whatever is
/// legitimately there.
fn chunk_coord(
    coord: Option<&fb::ChunkCoord>,
    kind: &'static str,
) -> Result<ChunkCoord, DecodeError> {
    let coord = coord.ok_or(DecodeError::MissingCoord(kind))?;
    Ok(ChunkCoord {
        cx: coord.cx(),
        cy: coord.cy(),
        cz: coord.cz(),
    })
}

/// Copies a `BlockCoord` struct out of the buffer.
///
/// Absence is refused for the same reason [`chunk_coord`] refuses it, and the schema
/// says so in as many words: a struct field is optional in FlatBuffers like any
/// other, and the origin is a real location. An edit defaulted to `0, 0, 0` would
/// rewrite a voxel nobody named — and, unlike a missing chunk coordinate, it would
/// do so somewhere a player might well be standing.
fn block_coord(
    pos: Option<&fb::BlockCoord>,
    kind: &'static str,
) -> Result<BlockCoord, DecodeError> {
    let pos = pos.ok_or(DecodeError::MissingBlockPos(kind))?;
    Ok(BlockCoord {
        x: pos.x(),
        y: pos.y(),
        z: pos.z(),
    })
}

/// Copies an `EntitySnapshot` out of the buffer, enforcing the finite-float invariant
/// `schemas/player.fbs` documents for it.
///
/// An **absent** entity vector is read as an empty one, unlike `ChunkData.runs` where
/// absence is an error. The two differ because the invariants differ: a chunk with no
/// voxels is not a chunk, while a session that can see nobody is a perfectly ordinary
/// state, and FlatBuffers is free to omit an empty vector. There is nothing for absence
/// to mean here other than "no entities".
fn entity_snapshot(snapshot: &fb::EntitySnapshot) -> Result<Snapshot, DecodeError> {
    let mut entities = Vec::new();
    let mut player_ids = HashSet::new();

    if let Some(list) = snapshot.entities() {
        // Sized from the vector the verifier has already accepted, which is what makes
        // this length safe to allocate from at all.
        entities.reserve(list.len());
        for state in &list {
            let state = entity_state(state)?;
            player_ids.insert(state.entity_id);
            entities.push(state);
        }
    }

    let mut drops = Vec::new();
    let mut drop_ids = HashSet::new();
    let mut mob_ids = HashSet::new();
    if let Some(list) = snapshot.drops() {
        drops.reserve(list.len());
        for state in &list {
            let state = item_drop_state(state)?;
            if player_ids.contains(&state.entity_id) {
                return Err(DecodeError::PlayerDropEntityConflict(state.entity_id));
            }
            drop_ids.insert(state.entity_id);
            drops.push(state);
        }
    }

    let mut mobs = Vec::new();
    if let Some(list) = snapshot.mobs() {
        mobs.reserve(list.len());
        for state in list.iter() {
            let state = mob_state(&state)?;
            // Against players, against drops, and against the mobs already read: the
            // schema's "globally unique" is a claim about the whole snapshot, and an id
            // that names two things is a client that would spawn one body for both.
            if player_ids.contains(&state.entity_id)
                || drop_ids.contains(&state.entity_id)
                || mob_ids.contains(&state.entity_id)
            {
                return Err(DecodeError::MobEntityConflict(state.entity_id));
            }
            mob_ids.insert(state.entity_id);
            mobs.push(state);
        }
    }

    let mut structures = Vec::new();
    let mut structure_ids = HashSet::new();
    if let Some(list) = snapshot.structures() {
        structures.reserve(list.len());
        for state in list.iter() {
            let state = structure_state(&state)?;
            // Against every other kind of id in the snapshot and against the structures
            // already read, exactly as a mob is checked. An id that names two things is a
            // client that would draw one of them over the other.
            if player_ids.contains(&state.structure_id)
                || drop_ids.contains(&state.structure_id)
                || mob_ids.contains(&state.structure_id)
                || !structure_ids.insert(state.structure_id)
            {
                return Err(DecodeError::StructureEntityConflict(state.structure_id));
            }
            structures.push(state);
        }
    }

    Ok(Snapshot {
        server_tick: snapshot.server_tick(),
        entities,
        drops,
        mobs,
        self_vitals: player_vitals(&snapshot.self_vitals())?,
        structures,
        // Copied, not checked. See the field's own documentation: the bound is against a
        // number this function has never seen.
        tick_of_day: snapshot.tick_of_day(),
    })
}

/// Copies one structure out of a snapshot and enforces every invariant
/// `schemas/player.fbs` attaches to `StructureState`.
///
/// A table rather than a struct, so `anchor` arrives as an `Option` and absence has to be
/// answered. It is refused rather than defaulted to the origin, for the reason a
/// `BlockUpdate` without a position is: the origin is a real location, and a tent pitched
/// at 0,0,0 is a camp the server never placed.
fn structure_state(state: &fb::StructureState) -> Result<StructureState, DecodeError> {
    let structure_id = state.structure_id();
    if structure_id == 0 {
        return Err(DecodeError::StructureWithoutIdentity);
    }

    let kind = StructureKind::from_wire(state.kind()).ok_or(DecodeError::UnknownStructureEnum {
        structure_id,
        field: "kind",
        value: state.kind().0,
    })?;
    let facing = Facing::from_wire(state.facing()).ok_or(DecodeError::UnknownStructureEnum {
        structure_id,
        field: "facing",
        value: state.facing().0,
    })?;

    let anchor = state
        .anchor()
        .ok_or(DecodeError::MissingStructureAnchor(structure_id))?;

    // No zero check on the owner, and its absence is the contract rather than an
    // omission. `schemas/player.fbs` makes `0` legal in this one field from V5 on:
    // it says the owner has no live session right now, not that the structure is
    // unowned. Ownership is keyed server-side by the identity behind
    // `ClientHello.player_token`, which never crosses the wire; this field is only
    // that identity's handle for one session, and an offline owner has no handle.
    //
    // `player/structures.rs` compares it against this session's own entity id to
    // decide whose camp is whose, and that comparison stays correct unchanged
    // *because of the check above*: `session_params` refuses a welcome whose own
    // `entity_id` is 0, so an offline owner matches nobody and the structure draws
    // as someone else's. Without that check this relaxation would hand every
    // offline owner's camp to a session the server had numbered 0.
    let owner_entity_id = state.owner_entity_id();

    Ok(StructureState {
        structure_id,
        kind,
        anchor: BlockCoord {
            x: anchor.x(),
            y: anchor.y(),
            z: anchor.z(),
        },
        facing,
        owner_entity_id,
    })
}

/// Copies one mob out of a snapshot and enforces every invariant `schemas/player.fbs`
/// attaches to `MobState`.
///
/// A table rather than a struct, so `pos` and `vel` arrive as `Option` and absence has
/// to be answered. It is refused rather than defaulted to the origin, for the reason a
/// `BlockUpdate` without a position is: the origin is a real location, and a draugr
/// standing at 0,0,0 is a creature the server never placed.
fn mob_state(state: &fb::MobState) -> Result<MobState, DecodeError> {
    let entity_id = state.entity_id();
    if entity_id == 0 {
        return Err(DecodeError::MobWithoutIdentity);
    }

    let checked = |field: &'static str, value: f32| -> Result<f32, DecodeError> {
        if value.is_finite() {
            Ok(value)
        } else {
            Err(DecodeError::NonFiniteMob {
                entity_id,
                field,
                value,
            })
        }
    };

    let pos = state.pos().ok_or(DecodeError::MissingMobTransform {
        entity_id,
        field: "pos",
    })?;
    let vel = state.vel().ok_or(DecodeError::MissingMobTransform {
        entity_id,
        field: "vel",
    })?;

    let kind = MobKind::from_wire(state.kind()).ok_or(DecodeError::UnknownMobEnum {
        entity_id,
        field: "kind",
        value: state.kind().0,
    })?;
    let action = MobAction::from_wire(state.action()).ok_or(DecodeError::UnknownMobEnum {
        entity_id,
        field: "action",
        value: state.action().0,
    })?;

    let (health, max_health) = (state.health(), state.max_health());
    if max_health == 0 || health > max_health {
        return Err(DecodeError::MobHealth {
            entity_id,
            health,
            max_health,
        });
    }

    Ok(MobState {
        entity_id,
        kind,
        pos: [
            checked("pos.x", pos.x())?,
            checked("pos.y", pos.y())?,
            checked("pos.z", pos.z())?,
        ],
        vel: [
            checked("vel.x", vel.x())?,
            checked("vel.y", vel.y())?,
            checked("vel.z", vel.z())?,
        ],
        yaw: checked("yaw", state.yaw())?,
        health,
        max_health,
        action,
    })
}

/// Copies the recipient's own vitals out of a snapshot.
///
/// The field is `(required)` on the wire, so an absent one never reaches here: the
/// verifier [`decode`] runs has already refused the buffer. What is left are the value
/// invariants, and they are the ones a health bar depends on — `max_health` is what it
/// divides by.
fn player_vitals(vitals: &fb::PlayerVitals) -> Result<PlayerVitals, DecodeError> {
    let life_state =
        LifeState::from_wire(vitals.life_state()).ok_or(DecodeError::UnknownLifeState)?;

    let (health, max_health) = (vitals.health(), vitals.max_health());
    if max_health == 0 || health > max_health {
        return Err(DecodeError::VitalsHealth { health, max_health });
    }
    if life_state == LifeState::Alive && health == 0 {
        return Err(DecodeError::AliveWithoutHealth);
    }

    let respawn_ticks = vitals.respawn_ticks();
    if respawn_ticks != 0 && life_state != LifeState::Dead {
        return Err(DecodeError::RespawnWhileAlive { respawn_ticks });
    }

    Ok(PlayerVitals {
        health,
        max_health,
        life_state,
        respawn_ticks,
        invulnerable: vitals.invulnerable(),
    })
}

/// Copies one entity out of a snapshot, refusing a non-finite component.
///
/// A finiteness test, never a clamp. NaN compares false against every bound, so a clamp
/// would pass one through untouched — into the interpolation, then into a `Transform`,
/// and from there into every child of it.
fn entity_state(state: &fb::EntityState) -> Result<EntityState, DecodeError> {
    let entity_id = state.entity_id();
    let pos = state.pos();
    let vel = state.vel();

    let checked = |field: &'static str, value: f32| -> Result<f32, DecodeError> {
        if value.is_finite() {
            Ok(value)
        } else {
            Err(DecodeError::NonFiniteEntity {
                entity_id,
                field,
                value,
            })
        }
    };

    Ok(EntityState {
        entity_id,
        pos: [
            checked("pos.x", pos.x())?,
            checked("pos.y", pos.y())?,
            checked("pos.z", pos.z())?,
        ],
        vel: [
            checked("vel.x", vel.x())?,
            checked("vel.y", vel.y())?,
            checked("vel.z", vel.z())?,
        ],
        yaw: checked("yaw", state.yaw())?,
    })
}

/// Copies one item drop out of a snapshot and enforces the invariants attached
/// to `ItemDropState` in the schema.
fn item_drop_state(state: &fb::ItemDropState) -> Result<ItemDropState, DecodeError> {
    let entity_id = state.entity_id();
    let pos = state.pos();
    let checked = |field: &'static str, value: f32| -> Result<f32, DecodeError> {
        if value.is_finite() {
            Ok(value)
        } else {
            Err(DecodeError::NonFiniteDrop {
                entity_id,
                field,
                value,
            })
        }
    };

    let item_id = state.item_id();
    if item_id == 0 {
        return Err(DecodeError::DropWithoutItem(entity_id));
    }
    let count = state.count();
    if count == 0 {
        return Err(DecodeError::EmptyDrop(entity_id));
    }

    Ok(ItemDropState {
        entity_id,
        pos: [
            checked("pos.x", pos.x())?,
            checked("pos.y", pos.y())?,
            checked("pos.z", pos.z())?,
        ],
        item_id,
        count,
    })
}

/// Copies a `ServerWelcome` out of the buffer, enforcing every decoder invariant
/// `schemas/handshake.fbs` documents.
///
/// The order matters as much as the checks: nothing is derived from a field
/// before that field has been accepted. An honestly buggy server reaches a
/// division by zero exactly as easily as a hostile one, so trusting the server
/// on gameplay outcomes does not extend to trusting it on array bounds.
fn session_params(welcome: &fb::ServerWelcome) -> Result<SessionParams, DecodeError> {
    // Zero is the reserved id, exactly as it is for a mob and for a player in a
    // snapshot. See `DecodeError::WelcomeEntityId` for why this one is load-bearing
    // rather than tidy: `structure_state` accepts owner 0 from V5 on, and the
    // comparison that reads it is against *this* number.
    let entity_id = welcome.entity_id();
    if entity_id == 0 {
        return Err(DecodeError::WelcomeEntityId);
    }

    let spawn = welcome.spawn().ok_or(DecodeError::MissingSpawn)?;
    let spawn = [spawn.x(), spawn.y(), spawn.z()];
    for (axis, value) in spawn.iter().enumerate() {
        // A finiteness test, deliberately not a range clamp: NaN compares false
        // against every bound, so a clamp would pass it through untouched and it
        // would propagate through every transform downstream.
        if !value.is_finite() {
            return Err(DecodeError::NonFiniteSpawn {
                axis,
                value: *value,
            });
        }
    }

    let tick_rate = welcome.tick_rate();
    if tick_rate == 0 {
        return Err(DecodeError::TickRate(tick_rate));
    }

    let chunk_size = welcome.chunk_size();
    if !(1..=MAX_CHUNK_SIZE).contains(&chunk_size) {
        return Err(DecodeError::ChunkSize(chunk_size));
    }

    let view_distance = welcome.view_distance();
    if view_distance > MAX_VIEW_DISTANCE {
        return Err(DecodeError::ViewDistance(view_distance));
    }

    let inventory_slots = welcome.inventory_slots();
    if inventory_slots == 0 {
        return Err(DecodeError::InventorySlots(inventory_slots));
    }

    let hotbar_slots = welcome.hotbar_slots();
    if hotbar_slots == 0 {
        return Err(DecodeError::HotbarSlots(hotbar_slots));
    }
    if hotbar_slots > inventory_slots {
        return Err(DecodeError::HotbarExceedsInventory {
            hotbar: hotbar_slots,
            inventory: inventory_slots,
        });
    }

    // Checked like a zero `tick_rate`, because the contract asks for it in the same
    // words: present and exactly PLAYER_TOKEN_LEN bytes, with absent, empty and
    // any other length all protocol errors. A short token is not something to pad
    // and a long one is not something to truncate — either would store an identity
    // the server never issued and present it back on the next connection.
    let token = welcome
        .player_token()
        .ok_or(DecodeError::MissingPlayerToken)?;
    let token = token.bytes();
    if token.len() != PLAYER_TOKEN_LEN {
        return Err(DecodeError::PlayerTokenLength(token.len()));
    }
    let mut player_token = [0u8; PLAYER_TOKEN_LEN];
    // Copied out like every other field here: the accessor borrows the frame, and
    // the frame is gone by the time anything stores this.
    player_token.copy_from_slice(token);

    // The clock is all-or-nothing. A zero day length is a server announcing it keeps no
    // time of day, and the two boundaries are then not examined at all — checking them
    // would turn the legal pre-V6 shape (three absent scalars, which decode as three
    // zeros) into a protocol error and refuse every server that has not grown a clock
    // yet. A declared clock is held to the ordering the contract states.
    let clock = WorldClock {
        day_length_ticks: welcome.day_length_ticks(),
        night_start_ticks: welcome.night_start_ticks(),
        night_end_ticks: welcome.night_end_ticks(),
    };
    if clock.declared()
        && !(0 < clock.night_start_ticks
            && clock.night_start_ticks < clock.night_end_ticks
            && clock.night_end_ticks <= clock.day_length_ticks)
    {
        return Err(DecodeError::WorldClock {
            day_length: clock.day_length_ticks,
            night_start: clock.night_start_ticks,
            night_end: clock.night_end_ticks,
        });
    }

    Ok(SessionParams {
        entity_id,
        spawn,
        world_seed: welcome.world_seed(),
        tick_rate,
        chunk_size,
        view_distance,
        inventory_slots,
        hotbar_slots,
        player_token: PlayerToken::from_bytes(player_token),
        clock,
    })
}

/// Builds the first message this client sends.
///
/// The version is `ProtocolVersion::Current` from the generated bindings, never a
/// literal: the whole point of the enum is that both sides read the same symbol
/// out of the same contract.
///
/// `player_token` is whatever identity the caller holds for *this* server, and
/// `None` is a first connection. Presenting one is a claim and not a statement —
/// the server decides whether to honour it, and answers with the token that
/// actually applies. Nothing here reads the value: this function puts it on the
/// wire, and that is the entire client-side use of a token.
pub fn encode_client_hello(
    player_name: &str,
    player_token: Option<PlayerToken>,
    session_ticket: Option<SessionTicket>,
) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY * 2);

    // A string and a vector must both exist before the table that references them
    // opens.
    let name = builder.create_string(player_name);
    let token = player_token.map(|token| builder.create_vector(token.as_bytes()));
    let ticket = session_ticket.map(|ticket| builder.create_vector(ticket.as_bytes()));
    let hello = fb::ClientHello::create(
        &mut builder,
        &fb::ClientHelloArgs {
            protocol_version: fb::ProtocolVersion::Current,
            player_name: Some(name),
            // Absent rather than empty on a first connection. The contract reads
            // both the same way, and absent is the one that says "nothing to
            // present" without putting a zero-length vector on the wire to say it.
            player_token: token,
            // Retired at V7 in the other direction: a client with an account writes
            // this and leaves `player_token` absent. Both are `Option` because both
            // are claims a client may not have to make, and this build has no account
            // service, so `None` is what `net/session.rs` passes today.
            session_ticket: ticket,
        },
    );

    finish_envelope(builder, fb::Payload::ClientHello, hello.as_union_value())
}

/// Builds the choice of an existing character.
///
/// The id is one the server minted and sent in a `ServerCharacterList`, which is the
/// one kind of identifier a client may echo back. Whether it names a character this
/// account owns is re-read server-side, so nothing here validates it.
pub fn encode_select_character_request(character_id: u64) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let payload = fb::SelectCharacterRequest::create(
        &mut builder,
        &fb::SelectCharacterRequestArgs { character_id },
    );

    finish_envelope(
        builder,
        fb::Payload::SelectCharacterRequest,
        payload.as_union_value(),
    )
}

/// Builds the request to make a new character.
///
/// The name is written verbatim, the empty string included: what names a server
/// accepts is the *server's* rule, answered with `RejectReason::CHARACTER_NAME_REFUSED`
/// — a refusal the player can read and act on. A client that pre-judged it would be
/// holding an opinion about a rule it does not own.
///
/// The appearance needs no such caveat: an [`Appearance`] cannot be constructed holding
/// a colour or a hair model the contract forbids, so this always writes a legal one.
pub fn encode_create_character_request(request: &CreateCharacterRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    // A string and a nested table must both exist before the table that references
    // them opens — unlike a struct, which is written inline while its parent is open.
    let name = builder.create_string(&request.name);
    let appearance = encode_appearance(&mut builder, request.appearance);
    let payload = fb::CreateCharacterRequest::create(
        &mut builder,
        &fb::CreateCharacterRequestArgs {
            name: Some(name),
            appearance: Some(appearance),
        },
    );

    finish_envelope(
        builder,
        fb::Payload::CreateCharacterRequest,
        payload.as_union_value(),
    )
}

/// Writes one appearance table and returns its offset.
///
/// Must be called while no other table is open: a nested table is reached through an
/// offset and has to be finished before its parent starts.
fn encode_appearance<'b>(
    builder: &mut FlatBufferBuilder<'b>,
    appearance: Appearance,
) -> flatbuffers::WIPOffset<fb::Appearance<'b>> {
    fb::Appearance::create(
        builder,
        &fb::AppearanceArgs {
            skin_color: appearance.skin_color(),
            shirt_color: appearance.shirt_color(),
            trousers_color: appearance.trousers_color(),
            shoes_color: appearance.shoes_color(),
            hair_model: appearance.hair_model().wire(),
            hair_color: appearance.hair_color(),
        },
    )
}

/// Builds one tick of intent.
///
/// Every float is written through [`finite_or_zero`]. The finite-float invariant is a
/// property of the *contract*, not of one direction of it: the client refuses a
/// non-finite position from the server, and the server discarding a non-finite axis is no
/// licence to send one.
pub fn encode_player_input(input: &PlayerInput) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let payload = fb::PlayerInput::create(
        &mut builder,
        &fb::PlayerInputArgs {
            client_tick: input.client_tick,
            move_x: finite_or_zero(input.move_x),
            move_z: finite_or_zero(input.move_z),
            yaw: finite_or_zero(input.yaw),
            pitch: finite_or_zero(input.pitch),
            jump: input.jump,
        },
    );

    finish_envelope(builder, fb::Payload::PlayerInput, payload.as_union_value())
}

/// Builds one request to change a voxel.
///
/// No float in it, and therefore no finiteness question: a block address is three
/// integers. That is a property of the contract rather than an accident — see the
/// argument for `BlockCoord` being ints in `schemas/world.fbs`.
///
/// The action is always `Break` or `Place`, because [`EditAction`] has no third
/// variant to encode. `Unknown` reaches the server only from a client that omitted
/// the field, and the server refuses it rather than guessing.
pub fn encode_block_edit_request(request: &BlockEditRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::BlockEditRequestBuilder::new(&mut builder);
    // A struct field is written inline, so it is created while its parent table is
    // open — unlike a string or a vector, which must exist before.
    table.add_pos(&fb::BlockCoord::new(
        request.pos.x,
        request.pos.y,
        request.pos.z,
    ));
    table.add_action(request.action.wire());
    table.add_slot(request.slot);
    table.add_client_tick(request.client_tick);
    let payload = table.finish();

    finish_envelope(
        builder,
        fb::Payload::BlockEditRequest,
        payload.as_union_value(),
    )
}

/// Builds mining intent. The server owns progress and completion; `active`
/// merely says whether the control is currently held for this voxel.
pub fn encode_mine_request(request: &MineRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::MineRequestBuilder::new(&mut builder);
    table.add_pos(&fb::BlockCoord::new(
        request.pos.x,
        request.pos.y,
        request.pos.z,
    ));
    table.add_active(request.active);
    table.add_client_tick(request.client_tick);
    table.add_slot(request.slot);
    let payload = table.finish();

    finish_envelope(builder, fb::Payload::MineRequest, payload.as_union_value())
}

/// Builds intent to move items between authoritative inventory slots.
/// Callers take both bounds from `SessionParams.inventory_slots`; the server
/// independently rejects out-of-range slots and a zero count.
/// Builds one swing.
///
/// The whole message: an authoritative inventory slot and this client's own tick counter.
/// There is no target, no position, no aim and no damage, because the server chooses all
/// four from state it already owns — see `schemas/player.fbs`. Sending one is a request,
/// and the only reply is the next snapshot.
pub fn encode_attack_request(request: &AttackRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::AttackRequestBuilder::new(&mut builder);
    table.add_slot(request.slot);
    table.add_client_tick(request.client_tick);
    let payload = table.finish();

    finish_envelope(
        builder,
        fb::Payload::AttackRequest,
        payload.as_union_value(),
    )
}

/// Builds one craft intent.
///
/// The whole message: a recipe identity and this client's own tick counter. There is no
/// ingredient list and no product, because the server reads both from its own table — and
/// no station either, because whether the player is standing near a forge is something the
/// server can see and this client can only guess at. Sending one is a request, and the
/// only reply is the complete `InventoryState` that follows an accepted craft; a refused
/// one is silence, exactly as it is for [`encode_block_edit_request`].
///
/// [`RecipeId`] has no `Unknown`, so the `recipe` field is never the absent-field zero the
/// server fails closed on — the type makes that unrepresentable rather than a promise.
pub fn encode_craft_request(request: &CraftRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::CraftRequestBuilder::new(&mut builder);
    table.add_recipe(request.recipe.wire());
    table.add_client_tick(request.client_tick);
    let payload = table.finish();

    finish_envelope(builder, fb::Payload::CraftRequest, payload.as_union_value())
}

/// Builds one repair intent.
///
/// The whole message: two authoritative slot indexes and this client's own tick counter.
/// No durability, no kit identity and no restored amount — every one of those is read
/// server-side from the slots the indexes name, which is what makes a modified client
/// sending this frame no better off than an honest one. Whether the kit is a kit and
/// whether the target can be worn are the same decision, and it is not made here.
///
/// The indexes are sent exactly as given, out-of-range values included, because
/// `schemas/player.fbs` asks for that: a slot past the end of the pack is an ordinary
/// refusal in the simulation rather than a malformed frame. A refused mend is silence, and
/// an accepted one arrives as the complete `InventoryState` that follows.
pub fn encode_repair_request(request: &RepairRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::RepairRequestBuilder::new(&mut builder);
    table.add_kit_slot(request.kit_slot);
    table.add_target_slot(request.target_slot);
    table.add_client_tick(request.client_tick);
    let payload = table.finish();

    finish_envelope(
        builder,
        fb::Payload::RepairRequest,
        payload.as_union_value(),
    )
}

/// Builds one request to plant a structure.
///
/// No float in it, and therefore no finiteness question: an anchor is three integers and
/// a facing is one of four members. The quantization from the camera's yaw happens before
/// this — see `player::structures` — precisely so that nothing continuous crosses the
/// wire for a thing that sits on the voxel grid.
pub fn encode_place_structure_request(request: &PlaceStructureRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::PlaceStructureRequestBuilder::new(&mut builder);
    // A struct field is written inline, so it is created while its parent table is open —
    // unlike a string or a vector, which must exist before.
    table.add_anchor(&fb::BlockCoord::new(
        request.anchor.x,
        request.anchor.y,
        request.anchor.z,
    ));
    table.add_slot(request.slot);
    table.add_facing(request.facing.wire());
    table.add_client_tick(request.client_tick);
    let payload = table.finish();

    finish_envelope(
        builder,
        fb::Payload::PlaceStructureRequest,
        payload.as_union_value(),
    )
}

/// Builds one request to take a placed structure back.
///
/// The whole message is an id the server minted and this client's tick counter. Whether
/// the id names a standing structure this player may remove, and whether they are close
/// enough to it, are re-read from the authoritative registry — so echoing an id gains a
/// modified client nothing, and a refusal is silence.
pub fn encode_remove_structure_request(request: &RemoveStructureRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::RemoveStructureRequestBuilder::new(&mut builder);
    table.add_structure_id(request.structure_id);
    table.add_client_tick(request.client_tick);
    let payload = table.finish();

    finish_envelope(
        builder,
        fb::Payload::RemoveStructureRequest,
        payload.as_union_value(),
    )
}

pub fn encode_inventory_move_request(request: &InventoryMoveRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let payload = fb::InventoryMoveRequest::create(
        &mut builder,
        &fb::InventoryMoveRequestArgs {
            from: request.from,
            to: request.to,
            count: request.count,
        },
    );

    finish_envelope(
        builder,
        fb::Payload::InventoryMoveRequest,
        payload.as_union_value(),
    )
}

/// Builds one request for a chunk this client has lost.
///
/// **A request for data, never for an outcome**, and the weakest thing this client sends:
/// a coordinate, and nothing else. Whether this session may have the chunk, what the chunk
/// contains, and whether the ask is honoured at all are the server's answers —
/// `schemas/world.fbs` records the rules it applies, including a per-session rate limit,
/// and a refusal is silence exactly as it is for [`encode_block_edit_request`]. There is
/// no request id because there is no reply to correlate one with, and a client must never
/// read its own request as a promise.
///
/// No float in it and therefore no finiteness question: a chunk address is three integers.
pub fn encode_chunk_resend_request(coord: ChunkCoord) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::ChunkResendRequestBuilder::new(&mut builder);
    // A struct field is written inline, so it is created while its parent table is open —
    // unlike a string or a vector, which must exist before.
    table.add_coord(&fb::ChunkCoord::new(coord.cx, coord.cy, coord.cz));
    let payload = table.finish();

    finish_envelope(
        builder,
        fb::Payload::ChunkResendRequest,
        payload.as_union_value(),
    )
}

/// A finite value, or zero.
///
/// Zeroed rather than clamped, and zeroed rather than refused: this is the sending side,
/// where a NaN can only come from arithmetic inside this client, and "no intent this
/// tick" is the honest thing to send when the client has lost track of what the intent
/// was. Refusing would mean skipping a tick, which reads to the server as a released key.
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// Wraps a built payload in the one root type on the wire and stamps the file
/// identifier, so everything this module encodes passes the peer's tag check.
fn finish_envelope(
    mut builder: FlatBufferBuilder<'_>,
    kind: fb::Payload,
    payload: flatbuffers::WIPOffset<flatbuffers::UnionWIPOffset>,
) -> Vec<u8> {
    let envelope = fb::Envelope::create(
        &mut builder,
        &fb::EnvelopeArgs {
            payload_type: kind,
            payload: Some(payload),
        },
    );
    fb::finish_envelope_buffer(&mut builder, envelope);
    builder.finished_data().to_vec()
}

/// Encoders for messages only the **server** sends.
///
/// Test-only on purpose. They exist so the client's tests and its in-process stub
/// server can produce exactly the bytes `voxelheimd` produces, without a second
/// encoder that could drift from the contract — the mirror of
/// `protocol.EncodeClientHello` on the server, which exists so the server's tests
/// can produce a client's input.
#[cfg(test)]
pub(super) mod server_side {
    use super::{FlatBufferBuilder, MAX_CHUNK_SIZE, WorldClock, fb, finish_envelope};

    /// The token [`WelcomeWire::default`] carries: a legal one, so a test that is
    /// not about identity never has to name it.
    pub const DEFAULT_TOKEN: [u8; super::PLAYER_TOKEN_LEN] = [0x5a; super::PLAYER_TOKEN_LEN];

    /// A `ServerWelcome` as it sits on the wire, before validation.
    ///
    /// Every field is settable, including into states a correct server never
    /// produces — that is the point: the decoder's invariants need inputs that
    /// violate them. [`Default`] mirrors what `voxelheimd` actually sends, so a
    /// test only names the field it is breaking.
    ///
    /// Not `Copy`, since `player_token` has to be able to be any length at all —
    /// including none, which no fixed-size array can express.
    #[derive(Debug, Clone)]
    pub struct WelcomeWire {
        pub entity_id: u64,
        /// `None` omits the field entirely, which is how an absent struct field
        /// reaches the decoder.
        pub spawn: Option<[f32; 3]>,
        pub world_seed: i64,
        pub tick_rate: u8,
        pub chunk_size: u16,
        pub view_distance: u8,
        pub inventory_slots: u8,
        pub hotbar_slots: u8,
        /// `None` omits the field; any other length than
        /// [`super::PLAYER_TOKEN_LEN`] is a token the decoder must refuse, the
        /// empty vector included.
        pub player_token: Option<Vec<u8>>,
        /// The three clock scalars, written verbatim and never validated here: this
        /// helper is how a *peer* emits a welcome, including one whose clock is
        /// nonsense, which is the case the decoder exists for.
        pub clock: WorldClock,
    }

    impl Default for WelcomeWire {
        fn default() -> Self {
            // The values in server/cmd/voxelheimd/main.go and
            // server/internal/game.
            Self {
                entity_id: 1,
                spawn: Some([0.5, 80.0, 0.5]),
                world_seed: 1,
                tick_rate: 20,
                chunk_size: 32,
                view_distance: 8,
                inventory_slots: 36,
                hotbar_slots: 9,
                player_token: Some(DEFAULT_TOKEN.to_vec()),
                // No clock by default, which is what every server in this repository
                // announces today and therefore the shape most fixtures should carry.
                clock: WorldClock::default(),
            }
        }
    }

    impl WelcomeWire {
        /// The largest values the contract permits, so the boundary is covered
        /// from the accepting side as well as the rejecting one.
        pub fn at_the_limits() -> Self {
            Self {
                tick_rate: u8::MAX,
                chunk_size: MAX_CHUNK_SIZE,
                view_distance: super::MAX_VIEW_DISTANCE,
                ..Self::default()
            }
        }
    }

    /// Encodes a `ServerWelcome` envelope.
    pub fn encode_server_welcome(welcome: &WelcomeWire) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        // A vector must exist before the table that references it opens.
        let token = welcome
            .player_token
            .as_deref()
            .map(|token| builder.create_vector(token));
        let mut table = fb::ServerWelcomeBuilder::new(&mut builder);
        table.add_entity_id(welcome.entity_id);
        if let Some([x, y, z]) = welcome.spawn {
            // A struct field is written inline, so it must be created while its
            // parent table is open — unlike a string, which must exist before.
            table.add_spawn(&fb::Vec3::new(x, y, z));
        }
        table.add_world_seed(welcome.world_seed);
        table.add_tick_rate(welcome.tick_rate);
        table.add_chunk_size(welcome.chunk_size);
        table.add_view_distance(welcome.view_distance);
        table.add_inventory_slots(welcome.inventory_slots);
        table.add_hotbar_slots(welcome.hotbar_slots);
        if let Some(token) = token {
            table.add_player_token(token);
        }
        table.add_day_length_ticks(welcome.clock.day_length_ticks);
        table.add_night_start_ticks(welcome.clock.night_start_ticks);
        table.add_night_end_ticks(welcome.clock.night_end_ticks);
        let payload = table.finish();

        finish_envelope(
            builder,
            fb::Payload::ServerWelcome,
            payload.as_union_value(),
        )
    }

    /// Encodes a `ServerReject` envelope.
    pub fn encode_server_reject(reason: fb::RejectReason, detail: &str) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let detail = builder.create_string(detail);
        let reject = fb::ServerReject::create(
            &mut builder,
            &fb::ServerRejectArgs {
                reason,
                detail: Some(detail),
            },
        );

        finish_envelope(builder, fb::Payload::ServerReject, reject.as_union_value())
    }

    /// Encodes an `ActionRefused` envelope.
    ///
    /// The action and the reason are passed through as raw wire members, values no member
    /// of this build has included: a server one contract ahead is exactly the input the
    /// decoder's `Unknown` exists for, and a helper that only accepted named members
    /// could not build it. `anchor` of `None` omits the field entirely, which is how an
    /// absent struct field reaches the decoder. Mirrors `protocol.EncodeActionRefused`.
    pub fn encode_action_refused(
        action: fb::RefusedAction,
        reason: fb::RefusalReason,
        anchor: Option<[i32; 3]>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let mut table = fb::ActionRefusedBuilder::new(&mut builder);
        table.add_action(action);
        table.add_reason(reason);
        if let Some([x, y, z]) = anchor {
            // A struct field is written inline, so it is created while its parent table is
            // open — unlike a string or a vector, which must exist before.
            table.add_anchor(&fb::BlockCoord::new(x, y, z));
        }
        let payload = table.finish();

        finish_envelope(
            builder,
            fb::Payload::ActionRefused,
            payload.as_union_value(),
        )
    }

    /// Encodes a `ChunkData` envelope from raw runs.
    ///
    /// `runs` is passed through untouched, invalid shapes included: the decoder's
    /// invariants need inputs that violate them, and a helper that quietly fixed
    /// the payload would test nothing. Mirrors `protocol.EncodeChunkData`.
    pub fn encode_chunk_data(coord: [i32; 3], runs: &[u16]) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        // The vector must exist before the table that references it opens.
        let runs = builder.create_vector(runs);
        let mut table = fb::ChunkDataBuilder::new(&mut builder);
        table.add_coord(&fb::ChunkCoord::new(coord[0], coord[1], coord[2]));
        table.add_runs(runs);
        let payload = table.finish();

        finish_envelope(builder, fb::Payload::ChunkData, payload.as_union_value())
    }

    /// Encodes a `ChunkUnload` envelope. Mirrors `protocol.EncodeChunkUnload`.
    pub fn encode_chunk_unload(coord: [i32; 3]) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let mut table = fb::ChunkUnloadBuilder::new(&mut builder);
        table.add_coord(&fb::ChunkCoord::new(coord[0], coord[1], coord[2]));
        let payload = table.finish();

        finish_envelope(builder, fb::Payload::ChunkUnload, payload.as_union_value())
    }

    /// Encodes a `ChunkData` envelope with no fields set at all, which is how an
    /// absent coord and absent runs reach the decoder.
    pub fn encode_bare_chunk_data() -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload = fb::ChunkDataBuilder::new(&mut builder).finish();
        finish_envelope(builder, fb::Payload::ChunkData, payload.as_union_value())
    }

    /// Encodes a `ChunkUnload` envelope with no coord.
    pub fn encode_bare_chunk_unload() -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload = fb::ChunkUnloadBuilder::new(&mut builder).finish();
        finish_envelope(builder, fb::Payload::ChunkUnload, payload.as_union_value())
    }

    /// Encodes a `BlockUpdate` envelope. Mirrors `protocol.EncodeBlockUpdate`.
    ///
    /// `block_id` is passed through untouched, ids this build has no colour for
    /// included: a server one contract ahead is a state the decoder has to carry
    /// rather than repair.
    pub fn encode_block_update(pos: [i32; 3], block_id: u16) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let mut table = fb::BlockUpdateBuilder::new(&mut builder);
        table.add_pos(&fb::BlockCoord::new(pos[0], pos[1], pos[2]));
        table.add_block_id(block_id);
        let payload = table.finish();

        finish_envelope(builder, fb::Payload::BlockUpdate, payload.as_union_value())
    }

    /// Encodes a `BlockUpdate` with no fields set at all, which is how an absent
    /// `pos` reaches the decoder.
    pub fn encode_bare_block_update() -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload = fb::BlockUpdateBuilder::new(&mut builder).finish();
        finish_envelope(builder, fb::Payload::BlockUpdate, payload.as_union_value())
    }

    /// Encodes an `InventoryState` envelope. `None` omits the vector; invalid pair
    /// shapes are passed through untouched so the decoder's invariants can be tested.
    pub fn encode_inventory_state(stacks: Option<&[u16]>) -> Vec<u8> {
        // Matched all-zero durability vectors, derived from the pairs: this is the shape
        // every inventory of pure resources has, and the one a test that is not about
        // durability means. Deriving them rather than asking for them is what keeps the
        // three vectors aligned by construction here.
        let zeros = vec![0u16; stacks.map_or(0, |stacks| stacks.len() / 2)];
        encode_inventory_state_with_durability(stacks, Some(&zeros), Some(&zeros))
    }

    /// Encodes an `InventoryState` with all three vectors settable independently,
    /// including into lengths and pairs a correct server never emits — that is the
    /// point: the decoder's alignment invariant needs inputs that violate it.
    pub fn encode_inventory_state_with_durability(
        stacks: Option<&[u16]>,
        durability: Option<&[u16]>,
        max_durability: Option<&[u16]>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let stacks = stacks.map(|stacks| builder.create_vector(stacks));
        let durability = durability.map(|values| builder.create_vector(values));
        let max_durability = max_durability.map(|values| builder.create_vector(values));
        let inventory = fb::InventoryState::create(
            &mut builder,
            &fb::InventoryStateArgs {
                stacks,
                durability,
                max_durability,
            },
        );
        finish_envelope(
            builder,
            fb::Payload::InventoryState,
            inventory.as_union_value(),
        )
    }

    /// One entity as it sits on the wire, before validation.
    ///
    /// Every field is settable, including into states a correct server never produces —
    /// that is the point: the decoder's finite-float invariant needs inputs that violate
    /// it. Mirrors `protocol.EntityState` on the server.
    #[derive(Debug, Clone, Copy)]
    pub struct EntityStateWire {
        pub entity_id: u64,
        pub pos: [f32; 3],
        pub vel: [f32; 3],
        pub yaw: f32,
    }

    impl EntityStateWire {
        /// An entity standing still at the origin, for a test to break one field of.
        pub fn at(entity_id: u64, x: f32) -> Self {
            Self {
                entity_id,
                pos: [x, 64.0, 0.0],
                vel: [0.0, 0.0, 0.0],
                yaw: 0.0,
            }
        }
    }

    /// One item drop as it sits on the wire, before validation.
    #[derive(Debug, Clone, Copy)]
    pub struct ItemDropStateWire {
        pub entity_id: u64,
        pub pos: [f32; 3],
        pub item_id: u16,
        pub count: u16,
    }

    impl ItemDropStateWire {
        /// A valid drop at a useful non-origin position, for a test to break one field of.
        pub fn item(entity_id: u64, item_id: u16) -> Self {
            Self {
                entity_id,
                pos: [1.5, 64.25, -2.0],
                item_id,
                count: 1,
            }
        }
    }

    /// One mob as it sits on the wire, before validation.
    ///
    /// `pos` and `vel` are `Option` because `MobState` is a *table*: a struct field in a
    /// table is optional like any other, and `None` omits it entirely, which is how an
    /// absent transform reaches the decoder.
    #[derive(Debug, Clone, Copy)]
    pub struct MobStateWire {
        pub entity_id: u64,
        pub kind: fb::MobKind,
        pub pos: Option<[f32; 3]>,
        pub vel: Option<[f32; 3]>,
        pub yaw: f32,
        pub health: u16,
        pub max_health: u16,
        pub action: fb::MobAction,
    }

    impl MobStateWire {
        /// A valid draugr, for a test to break one field of.
        pub fn draugr(entity_id: u64, x: f32) -> Self {
            Self {
                entity_id,
                kind: fb::MobKind::Draugr,
                pos: Some([x, 64.0, 0.0]),
                vel: Some([0.0, 0.0, 0.0]),
                yaw: 0.0,
                health: 60,
                max_health: 60,
                action: fb::MobAction::Idle,
            }
        }
    }

    /// One structure as it sits on the wire, before validation.
    ///
    /// `anchor` is an `Option` because `StructureState` is a *table*: a struct field in a
    /// table is optional like any other, and `None` omits it entirely, which is how an
    /// absent anchor reaches the decoder.
    #[derive(Debug, Clone, Copy)]
    pub struct StructureStateWire {
        pub structure_id: u64,
        pub kind: fb::StructureKind,
        pub anchor: Option<[i32; 3]>,
        pub facing: fb::Facing,
        pub owner_entity_id: u64,
    }

    impl StructureStateWire {
        /// A valid tent, for a test to break one field of.
        pub fn tent(structure_id: u64, owner_entity_id: u64) -> Self {
            Self {
                structure_id,
                kind: fb::StructureKind::Tent,
                anchor: Some([4, 63, -7]),
                facing: fb::Facing::North,
                owner_entity_id,
            }
        }
    }

    /// The recipient's vitals as they sit on the wire, before validation.
    #[derive(Debug, Clone, Copy)]
    pub struct PlayerVitalsWire {
        pub health: u16,
        pub max_health: u16,
        pub life_state: fb::LifeState,
        pub respawn_ticks: u32,
        pub invulnerable: bool,
    }

    impl Default for PlayerVitalsWire {
        /// An unharmed living player, so a test only names the field it is breaking.
        fn default() -> Self {
            Self {
                health: 100,
                max_health: 100,
                life_state: fb::LifeState::Alive,
                respawn_ticks: 0,
                invulnerable: false,
            }
        }
    }

    /// Encodes an `EntitySnapshot` envelope. Mirrors `protocol.EncodeEntitySnapshot`,
    /// including the back-to-front vector build a struct vector needs.
    pub fn encode_entity_snapshot(server_tick: u32, entities: &[EntityStateWire]) -> Vec<u8> {
        encode_entity_snapshot_with_drops(server_tick, entities, &[])
    }

    /// Encodes the two struct entity kinds carried by an `EntitySnapshot`.
    pub fn encode_entity_snapshot_with_drops(
        server_tick: u32,
        entities: &[EntityStateWire],
        drops: &[ItemDropStateWire],
    ) -> Vec<u8> {
        encode_entity_snapshot_with(
            server_tick,
            entities,
            drops,
            &[],
            PlayerVitalsWire::default(),
            &[],
        )
    }

    /// Encodes every vector an `EntitySnapshot` carries, plus the recipient's vitals.
    ///
    /// The three narrower helpers above delegate here with a valid living player, so a
    /// test that is not about vitals never has to name them.
    pub fn encode_entity_snapshot_with(
        server_tick: u32,
        entities: &[EntityStateWire],
        drops: &[ItemDropStateWire],
        mobs: &[MobStateWire],
        vitals: PlayerVitalsWire,
        structures: &[StructureStateWire],
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(
            entities.len() * 40 + drops.len() * 24 + mobs.len() * 64 + structures.len() * 48 + 128,
        );

        // The vector must exist before the table that references it opens.
        let laid_out: Vec<fb::EntityState> = entities
            .iter()
            .map(|state| {
                fb::EntityState::new(
                    state.entity_id,
                    &fb::Vec3::new(state.pos[0], state.pos[1], state.pos[2]),
                    &fb::Vec3::new(state.vel[0], state.vel[1], state.vel[2]),
                    state.yaw,
                )
            })
            .collect();
        let entities = builder.create_vector(&laid_out);
        let laid_out: Vec<fb::ItemDropState> = drops
            .iter()
            .map(|state| {
                fb::ItemDropState::new(
                    state.entity_id,
                    &fb::Vec3::new(state.pos[0], state.pos[1], state.pos[2]),
                    state.item_id,
                    state.count,
                )
            })
            .collect();
        let drops = builder.create_vector(&laid_out);

        // A vector of *tables* holds offsets, so every mob table has to be finished
        // before the vector that points at them opens.
        let laid_out: Vec<_> = mobs
            .iter()
            .map(|mob| {
                let mut table = fb::MobStateBuilder::new(&mut builder);
                table.add_entity_id(mob.entity_id);
                table.add_kind(mob.kind);
                if let Some(pos) = mob.pos {
                    table.add_pos(&fb::Vec3::new(pos[0], pos[1], pos[2]));
                }
                if let Some(vel) = mob.vel {
                    table.add_vel(&fb::Vec3::new(vel[0], vel[1], vel[2]));
                }
                table.add_yaw(mob.yaw);
                table.add_health(mob.health);
                table.add_max_health(mob.max_health);
                table.add_action(mob.action);
                table.finish()
            })
            .collect();
        let mobs = builder.create_vector(&laid_out);

        // A vector of tables again, for the reason the mobs above are one.
        let laid_out: Vec<_> = structures
            .iter()
            .map(|structure| {
                let mut table = fb::StructureStateBuilder::new(&mut builder);
                table.add_structure_id(structure.structure_id);
                table.add_kind(structure.kind);
                if let Some([x, y, z]) = structure.anchor {
                    table.add_anchor(&fb::BlockCoord::new(x, y, z));
                }
                table.add_facing(structure.facing);
                table.add_owner_entity_id(structure.owner_entity_id);
                table.finish()
            })
            .collect();
        let structures = builder.create_vector(&laid_out);

        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
            },
        );

        let mut table = fb::EntitySnapshotBuilder::new(&mut builder);
        table.add_server_tick(server_tick);
        table.add_entities(entities);
        table.add_drops(drops);
        table.add_mobs(mobs);
        table.add_self_vitals(self_vitals);
        table.add_structures(structures);
        let payload = table.finish();

        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            payload.as_union_value(),
        )
    }

    /// Encodes an `EntitySnapshot` with no entity, drop, mob or structure vector at all,
    /// which is how an absent vector reaches the decoder. Vitals are present, because they are
    /// `(required)` and their absence is a different frame — see below.
    pub fn encode_bare_entity_snapshot(server_tick: u32) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let vitals = PlayerVitalsWire::default();
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
            },
        );

        let snapshot = fb::EntitySnapshot::create(
            &mut builder,
            &fb::EntitySnapshotArgs {
                tick_of_day: 0,
                server_tick,
                entities: None,
                drops: None,
                mobs: None,
                self_vitals: Some(self_vitals),
                structures: None,
            },
        );

        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            snapshot.as_union_value(),
        )
    }

    /// Encodes a snapshot carrying nothing but a tick and a time of day.
    ///
    /// Every vector empty, deliberately: what its callers are checking is one scalar's
    /// journey across the wire, and an entity in the frame would only be a second thing
    /// that could go wrong.
    pub fn encode_entity_snapshot_at_tick_of_day(server_tick: u32, tick_of_day: u32) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let vitals = PlayerVitalsWire::default();
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
            },
        );

        let snapshot = fb::EntitySnapshot::create(
            &mut builder,
            &fb::EntitySnapshotArgs {
                server_tick,
                entities: None,
                drops: None,
                mobs: None,
                self_vitals: Some(self_vitals),
                structures: None,
                tick_of_day,
            },
        );

        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            snapshot.as_union_value(),
        )
    }

    /// Encodes an `EntitySnapshot` with no `self_vitals`, which no generated builder
    /// will produce: `EntitySnapshotBuilder::finish` asserts the `(required)` field, and
    /// lacking it is this frame's entire purpose.
    ///
    /// Hand-rolled from the raw table API instead. That is not a trick to get around a
    /// safety check — it is how a *peer* would emit one, and the peer is the case the
    /// decoder exists for.
    pub fn encode_entity_snapshot_without_vitals(server_tick: u32) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let start = builder.start_table();
        builder.push_slot::<u32>(fb::EntitySnapshot::VT_SERVER_TICK, server_tick, 0);
        let snapshot = builder.end_table(start);

        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            snapshot.as_union_value(),
        )
    }

    /// Encodes `MineProgress`, optionally omitting its required position so the
    /// decoder's absent-struct invariant can be exercised.
    pub fn encode_mine_progress(pos: Option<[i32; 3]>, progress: u8) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let mut table = fb::MineProgressBuilder::new(&mut builder);
        if let Some([x, y, z]) = pos {
            table.add_pos(&fb::BlockCoord::new(x, y, z));
        }
        table.add_progress(progress);
        let payload = table.finish();
        finish_envelope(builder, fb::Payload::MineProgress, payload.as_union_value())
    }

    /// An `Appearance` as it sits on the wire, before validation.
    ///
    /// Every field is settable, including into states a correct server never produces
    /// — a colour with its reserved high byte set, a hair model no member has. That is
    /// the point: the decoder's invariants need inputs that violate them, and
    /// [`super::Appearance`] cannot express one.
    #[derive(Debug, Clone, Copy)]
    pub struct AppearanceWire {
        pub skin_color: u32,
        pub shirt_color: u32,
        pub trousers_color: u32,
        pub shoes_color: u32,
        pub hair_model: fb::HairModel,
        pub hair_color: u32,
    }

    impl Default for AppearanceWire {
        fn default() -> Self {
            Self {
                skin_color: 0x00E3_C4A0,
                shirt_color: 0x004A_5D3B,
                trousers_color: 0x002B_2118,
                shoes_color: 0x0055_3311,
                hair_model: fb::HairModel::Braided,
                hair_color: 0x00B0_7A32,
            }
        }
    }

    /// A `CharacterSummary` as it sits on the wire. `name` and `appearance` are
    /// `Option` so a test can omit either, which is how an absent field reaches the
    /// decoder.
    #[derive(Debug, Clone)]
    pub struct CharacterSummaryWire {
        pub character_id: u64,
        pub name: Option<String>,
        pub appearance: Option<AppearanceWire>,
    }

    impl Default for CharacterSummaryWire {
        fn default() -> Self {
            Self {
                character_id: 900,
                name: Some("Eivor".to_owned()),
                appearance: Some(AppearanceWire::default()),
            }
        }
    }

    fn appearance_offset<'b>(
        builder: &mut FlatBufferBuilder<'b>,
        appearance: AppearanceWire,
    ) -> ::flatbuffers::WIPOffset<fb::Appearance<'b>> {
        fb::Appearance::create(
            builder,
            &fb::AppearanceArgs {
                skin_color: appearance.skin_color,
                shirt_color: appearance.shirt_color,
                trousers_color: appearance.trousers_color,
                shoes_color: appearance.shoes_color,
                hair_model: appearance.hair_model,
                hair_color: appearance.hair_color,
            },
        )
    }

    /// A `ServerCharacterList` carrying exactly these summaries and this limit.
    ///
    /// `characters` of `None` omits the vector entirely, which the contract reads the
    /// same way as an empty one — and a test has to be able to build both to say so.
    pub fn encode_server_character_list(
        characters: Option<&[CharacterSummaryWire]>,
        max_characters: u8,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY * 4);

        let vector = characters.map(|characters| {
            // Every string and nested table must be finished before the vector that
            // carries its offset opens.
            let summaries: Vec<_> = characters
                .iter()
                .map(|summary| {
                    let name = summary.name.as_ref().map(|n| builder.create_string(n));
                    let appearance = summary
                        .appearance
                        .map(|appearance| appearance_offset(&mut builder, appearance));
                    fb::CharacterSummary::create(
                        &mut builder,
                        &fb::CharacterSummaryArgs {
                            character_id: summary.character_id,
                            name,
                            appearance,
                        },
                    )
                })
                .collect();
            builder.create_vector(&summaries)
        });

        let payload = fb::ServerCharacterList::create(
            &mut builder,
            &fb::ServerCharacterListArgs {
                characters: vector,
                max_characters,
            },
        );
        finish_envelope(
            builder,
            fb::Payload::ServerCharacterList,
            payload.as_union_value(),
        )
    }

    /// A `PlayerAppearance`. `appearance` of `None` omits the table, which is the
    /// frame the decoder must refuse rather than fill in with a placeholder.
    pub fn encode_player_appearance(entity_id: u64, appearance: Option<AppearanceWire>) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let appearance = appearance.map(|a| appearance_offset(&mut builder, a));
        let payload = fb::PlayerAppearance::create(
            &mut builder,
            &fb::PlayerAppearanceArgs {
                entity_id,
                appearance,
            },
        );
        finish_envelope(
            builder,
            fb::Payload::PlayerAppearance,
            payload.as_union_value(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::server_side::{
        AppearanceWire, CharacterSummaryWire, DEFAULT_TOKEN, EntityStateWire, ItemDropStateWire,
        MobStateWire, PlayerVitalsWire, StructureStateWire, WelcomeWire, encode_action_refused,
        encode_bare_block_update, encode_bare_chunk_data, encode_bare_chunk_unload,
        encode_bare_entity_snapshot, encode_block_update, encode_chunk_data, encode_chunk_unload,
        encode_entity_snapshot, encode_entity_snapshot_with, encode_entity_snapshot_with_drops,
        encode_entity_snapshot_without_vitals, encode_inventory_state,
        encode_inventory_state_with_durability, encode_mine_progress, encode_player_appearance,
        encode_server_character_list, encode_server_reject, encode_server_welcome,
    };
    use super::*;

    fn decode_welcome(welcome: &WelcomeWire) -> Result<Message, DecodeError> {
        decode(&encode_server_welcome(welcome))
    }

    /// A snapshot carrying exactly the given mobs and vitals, and nothing else.
    fn snapshot_of(
        mobs: &[MobStateWire],
        vitals: PlayerVitalsWire,
    ) -> Result<Message, DecodeError> {
        decode(&encode_entity_snapshot_with(1, &[], &[], mobs, vitals, &[]))
    }

    /// A snapshot carrying exactly the given structures, and nothing else.
    fn structures_of(structures: &[StructureStateWire]) -> Result<Message, DecodeError> {
        decode(&encode_entity_snapshot_with(
            1,
            &[],
            &[],
            &[],
            PlayerVitalsWire::default(),
            structures,
        ))
    }

    #[test]
    fn limits_match_the_server() {
        // protocol.MaxChunkSize and protocol.MaxViewDistance.
        assert_eq!(MAX_CHUNK_SIZE, 40);
        assert_eq!(MAX_VIEW_DISTANCE, 16);
    }

    /// V6 arrived adding no payload — appended table fields and appended enum members —
    /// and then gained one without moving. V7 appends four and *does* move the version,
    /// and the difference between the two decisions is the whole content of this comment.
    /// [`the_v7_enums_append_without_moving_what_came_before`] carries the other half: a
    /// moved union tag reinterprets every frame on the wire, where a moved enum member
    /// reinterprets one field inside one.
    ///
    /// **Tag 20 did not move the version and tags 21..24 do**, which is not a rule about
    /// how many members were added. Union members are append-only exactly so that
    /// appending one need not be a break: a peer reads a tag it has no name for as
    /// [`Message::Deferred`] and drops it. What matters is what dropping it costs. A
    /// dropped `ActionRefused` costs a player one explanation. Three of V7's four *are*
    /// the handshake, so a V6 peer that drops `ServerCharacterList` never chooses a
    /// character and waits for ever on a welcome that is not coming — which is precisely
    /// the mid-session decode failure `ProtocolVersion` exists to turn into a clean
    /// refusal at the handshake.
    #[test]
    fn protocol_v7_appends_four_union_tags_and_moves_to_seven() {
        assert_eq!(fb::ProtocolVersion::Unknown.0, 0);
        assert_eq!(fb::ProtocolVersion::Current.0, 7);
        for (tag, value) in [
            (fb::Payload::ClientHello, 1),
            (fb::Payload::ServerWelcome, 2),
            (fb::Payload::ServerReject, 3),
            (fb::Payload::ChunkData, 4),
            (fb::Payload::ChunkUnload, 5),
            (fb::Payload::PlayerInput, 6),
            (fb::Payload::EntitySnapshot, 7),
            (fb::Payload::BlockEditRequest, 8),
            (fb::Payload::BlockUpdate, 9),
            (fb::Payload::InventoryState, 10),
            (fb::Payload::ChunkResendRequest, 11),
            (fb::Payload::MineRequest, 12),
            (fb::Payload::MineProgress, 13),
            (fb::Payload::InventoryMoveRequest, 14),
            (fb::Payload::AttackRequest, 15),
            (fb::Payload::CraftRequest, 16),
            (fb::Payload::RepairRequest, 17),
            (fb::Payload::PlaceStructureRequest, 18),
            (fb::Payload::RemoveStructureRequest, 19),
            (fb::Payload::ActionRefused, 20),
            (fb::Payload::ServerCharacterList, 21),
            (fb::Payload::SelectCharacterRequest, 22),
            (fb::Payload::CreateCharacterRequest, 23),
            (fb::Payload::PlayerAppearance, 24),
        ] {
            assert_eq!(tag.0, value);
        }

        // Membership, not just ordering. A swing is still answered by the next snapshot
        // and nothing else, and so is a craft and a repair; a *refused* placement is now
        // answered by `ActionRefused`, and an accepted one is not — there is still no
        // acknowledgement payload anywhere in this contract, and the size of the union is
        // the only place that claim can be checked. The extra member is `NONE`, the
        // implicit zero every FlatBuffers union carries.
        assert_eq!(
            fb::Payload::ENUM_VALUES.len(),
            25,
            "a new union member needs a decision, not a test edit"
        );
    }

    /// What `decode` owes a payload once the frame has been verified.
    ///
    /// Three answers, and every member of the union is given exactly one of them in
    /// [`CLASSIFICATION`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Handling {
        /// A real arm reads the payload — so the result is a message with contents,
        /// or a [`DecodeError`] naming the invariant the payload broke. Which of the
        /// two is not a classification, so this sweep does not pin it.
        Consumed,
        /// Only a client sends it. Direction is a protocol rule rather than a type
        /// rule, so receiving one is a protocol error.
        ClientOnly,
        /// Carried by name and nothing else.
        Deferred,
    }

    /// Every member of `Payload`, in tag order, with the answer `decode` owes it.
    ///
    /// Written out rather than derived: a table computed from the same match it
    /// checks would agree with every bug that match contains, which is how the four
    /// client→server payloads Protocol V4 added came to be reported as deferred
    /// server→client ones. An entry here is the deliberate decision the fallback used
    /// to make on everyone's behalf, and adding a union member is not possible without
    /// making it — the length and the order are both asserted below.
    const CLASSIFICATION: [(fb::Payload, Handling); 25] = [
        (fb::Payload::NONE, Handling::Deferred),
        (fb::Payload::ClientHello, Handling::ClientOnly),
        (fb::Payload::ServerWelcome, Handling::Consumed),
        (fb::Payload::ServerReject, Handling::Consumed),
        (fb::Payload::ChunkData, Handling::Consumed),
        (fb::Payload::ChunkUnload, Handling::Consumed),
        (fb::Payload::PlayerInput, Handling::ClientOnly),
        (fb::Payload::EntitySnapshot, Handling::Consumed),
        (fb::Payload::BlockEditRequest, Handling::ClientOnly),
        (fb::Payload::BlockUpdate, Handling::Consumed),
        (fb::Payload::InventoryState, Handling::Consumed),
        (fb::Payload::ChunkResendRequest, Handling::ClientOnly),
        (fb::Payload::MineRequest, Handling::ClientOnly),
        (fb::Payload::MineProgress, Handling::Consumed),
        (fb::Payload::InventoryMoveRequest, Handling::ClientOnly),
        (fb::Payload::AttackRequest, Handling::ClientOnly),
        (fb::Payload::CraftRequest, Handling::ClientOnly),
        (fb::Payload::RepairRequest, Handling::ClientOnly),
        (fb::Payload::PlaceStructureRequest, Handling::ClientOnly),
        (fb::Payload::RemoveStructureRequest, Handling::ClientOnly),
        (fb::Payload::ActionRefused, Handling::Consumed),
        (fb::Payload::ServerCharacterList, Handling::Consumed),
        (fb::Payload::SelectCharacterRequest, Handling::ClientOnly),
        (fb::Payload::CreateCharacterRequest, Handling::ClientOnly),
        (fb::Payload::PlayerAppearance, Handling::Consumed),
    ];

    /// An envelope whose union tag is exactly `kind`, carrying an empty payload table.
    ///
    /// Every field of every payload table in this contract is optional, so one empty
    /// table verifies as any of them — which is what lets a single helper produce a
    /// frame for every named member *and* for a tag no member has, where no encoder
    /// could exist at all. A `Consumed` payload therefore reaches its arm and
    /// is usually refused there on a missing field, which is the decode boundary doing
    /// its job and not a classification.
    ///
    /// `NONE` is the one tag that cannot be given a payload: the verifier requires the
    /// discriminant and the value to agree, so the frame it names is the one with no
    /// payload at all — which is also the only `NONE` frame a peer could send.
    fn tagged_frame(kind: fb::Payload) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::new();
        if kind == fb::Payload::NONE {
            let envelope = fb::Envelope::create(&mut builder, &fb::EnvelopeArgs::default());
            fb::finish_envelope_buffer(&mut builder, envelope);
            return builder.finished_data().to_vec();
        }
        let empty = fb::ClientHello::create(&mut builder, &fb::ClientHelloArgs::default());
        finish_envelope(builder, kind, empty.as_union_value())
    }

    /// Every union member resolves to the variant its direction calls for.
    ///
    /// The sweep the four payloads of legacy PR 131 would have failed: they were client→server
    /// and decoded as `Deferred`, and nothing said so because the fallback had an
    /// answer for them.
    #[test]
    fn every_union_member_is_classified_deliberately() {
        assert_eq!(
            CLASSIFICATION.len(),
            fb::Payload::ENUM_VALUES.len(),
            "a new union member needs a decision in `decode`, not a fallback"
        );

        for (index, (kind, handling)) in CLASSIFICATION.into_iter().enumerate() {
            // Positional, so that a reordered union — the one mistake `envelope.fbs`
            // says no compiler can catch — fails here as well as in the tag test above.
            assert_eq!(kind, fb::Payload::ENUM_VALUES[index], "tag {index}");

            let name = kind.variant_name().expect("a known member has a name");
            let decoded = decode(&tagged_frame(kind));
            match handling {
                Handling::ClientOnly => assert_eq!(
                    decoded,
                    Ok(Message::ClientOnly(name)),
                    "{name} is client→server, so receiving one is a protocol error"
                ),
                Handling::Deferred => assert_eq!(
                    decoded,
                    Ok(Message::Deferred(name)),
                    "{name} is carried by name and nothing else"
                ),
                Handling::Consumed => assert!(
                    !matches!(decoded, Ok(Message::ClientOnly(_) | Message::Deferred(_))),
                    "{name} has an arm that reads it, so it is neither deferred nor \
                     refused for its direction; got {decoded:?}"
                ),
            }
        }
    }

    /// The fallback answers for tags this build cannot name, and for nothing else.
    ///
    /// This is the half of the bug that had no symptom. A fallback reachable for a
    /// known member produces no warning, no failed test and a plausible-looking
    /// diagnostic, so the only way to hold the rule is to make the arm distinguishable
    /// and then say it is unreachable — `decode` reports `UNKNOWN_VARIANT` there,
    /// which is what `variant_name` already returns nothing for.
    #[test]
    fn the_fallback_is_reachable_only_for_a_tag_this_build_cannot_name() {
        for kind in fb::Payload::ENUM_VALUES {
            assert_ne!(
                decode(&tagged_frame(*kind)),
                Ok(Message::Deferred(UNKNOWN_VARIANT)),
                "{kind:?} is a member this build knows and must not reach the fallback"
            );
        }

        // Exhaustive over the rest of the byte, because the tag is one byte and a peer
        // one contract ahead picks which one.
        for tag in (fb::Payload::ENUM_MAX + 1)..=u8::MAX {
            let kind = fb::Payload(tag);
            assert_eq!(kind.variant_name(), None, "tag {tag} names no member");
            assert_eq!(
                decode(&tagged_frame(kind)),
                Ok(Message::Deferred(UNKNOWN_VARIANT)),
                "tag {tag} is a member from a newer contract"
            );
        }
    }

    /// A refusal arrives with its action, its reason and the cell the request named.
    #[test]
    fn a_refusal_carries_the_answer_and_the_cell_it_is_about() {
        let frame = encode_action_refused(
            fb::RefusedAction::PlaceStructure,
            fb::RefusalReason::GroundIsAir,
            Some([-4, 63, 17]),
        );

        assert_eq!(
            decode(&frame),
            Ok(Message::ActionRefused(ActionRefused {
                action: RefusedAction::PlaceStructure,
                reason: RefusalReason::GroundIsAir,
                anchor: Some(BlockCoord {
                    x: -4,
                    y: 63,
                    z: 17
                }),
            }))
        );
    }

    /// A refusal whose request named no anchor decodes as `None`, never as the origin.
    ///
    /// The absent-struct-field rule, read in the server→client direction. `(0, 0, 0)` is a
    /// real place: a refusal that arrived pointing at it would tell a player the server
    /// said no to something at the bottom of the world, which is not what happened.
    #[test]
    fn a_refusal_with_no_anchor_is_absent_rather_than_the_origin() {
        let frame = encode_action_refused(
            fb::RefusedAction::PlaceStructure,
            fb::RefusalReason::MalformedNoAnchor,
            None,
        );

        assert_eq!(
            decode(&frame),
            Ok(Message::ActionRefused(ActionRefused {
                action: RefusedAction::PlaceStructure,
                reason: RefusalReason::MalformedNoAnchor,
                anchor: None,
            }))
        );
    }

    /// A refusal this build cannot read costs the sentence and nothing else.
    ///
    /// **The one message on this wire that must never fail a frame.** Every other decode
    /// error ends the session, which is right for a snapshot whose vitals are missing and
    /// exactly wrong here: a refusal is the least important thing a server sends, and a
    /// client that disconnected because it could not name a reason would answer "why did
    /// nothing happen" by making a great deal happen.
    ///
    /// The absent-field zero and a member from a newer contract both land on `Unknown`,
    /// which is what the contract says its zero means.
    #[test]
    fn a_refusal_this_build_cannot_read_is_unknown_rather_than_a_broken_frame() {
        for (action, reason) in [
            (fb::RefusedAction::Unknown, fb::RefusalReason::Unknown),
            (fb::RefusedAction(200), fb::RefusalReason(200)),
            // A value inside the gap the contract leaves between its two groups, which is
            // where a reason appended later most plausibly lands.
            (fb::RefusedAction(6), fb::RefusalReason(40)),
        ] {
            assert_eq!(
                decode(&encode_action_refused(action, reason, None)),
                Ok(Message::ActionRefused(ActionRefused {
                    action: RefusedAction::Unknown,
                    reason: RefusalReason::Unknown,
                    anchor: None,
                })),
                "action {action:?}, reason {reason:?}"
            );
        }
    }

    /// The refusal enums fail closed on zero, and the two groups stay on their own sides.
    ///
    /// The zero is load-bearing for the usual reason: FlatBuffers decodes an absent scalar
    /// as its type's zero, so a refusal that lost its reason must read as a code nobody can
    /// explain rather than as the ground being air.
    ///
    /// The grouping is a property of the *numbers*, so it is pinned as numbers on both
    /// sides — the Go half is `TestRefusalEnumsFailClosedAndKeepTheirTwoGroups`. What this
    /// side does with it is [`RefusalReason::is_client_defect`], which is a `match` rather
    /// than a comparison: an arithmetic test would classify a member appended in a contract
    /// this build has never read, which is a guess about a group it cannot see.
    #[test]
    fn the_refusal_reasons_keep_their_two_groups() {
        assert_eq!(fb::RefusedAction::Unknown.0, 0);
        assert_eq!(fb::RefusedAction::PlaceStructure.0, 1);
        assert_eq!(fb::RefusedAction::MineBlock.0, 2);
        assert_eq!(fb::RefusedAction::EditBlock.0, 3);
        assert_eq!(fb::RefusedAction::Craft.0, 4);
        assert_eq!(fb::RefusedAction::Repair.0, 5);
        // No member for a removal, and its absence is the decision: a refused removal is
        // silence on purpose, because a client that could tell "no such structure" from
        // "not yours" from "too far away" could map somebody else's camp by asking.
        assert_eq!(
            fb::RefusedAction::ENUM_VALUES.len(),
            6,
            "a removal is refused in silence by design"
        );

        assert_eq!(fb::RefusalReason::Unknown.0, 0);
        for (reason, value) in [
            (fb::RefusalReason::GroundNotGenerated, 1),
            (fb::RefusalReason::GroundIsAir, 2),
            (fb::RefusalReason::SpaceNotGenerated, 3),
            (fb::RefusalReason::SpaceBlocked, 4),
            (fb::RefusalReason::OutOfReach, 5),
            (fb::RefusalReason::PlayerIsDead, 6),
            (fb::RefusalReason::SlotEmpty, 7),
            (fb::RefusalReason::SlotUnusable, 8),
            (fb::RefusalReason::SlotChanged, 9),
            (fb::RefusalReason::InventoryBusy, 10),
            (fb::RefusalReason::TentAlreadyPlaced, 11),
            (fb::RefusalReason::MalformedNoAnchor, 64),
            (fb::RefusalReason::MalformedFacing, 65),
            (fb::RefusalReason::MalformedSlot, 66),
            (fb::RefusalReason::MalformedKind, 67),
        ] {
            assert_eq!(reason.0, value, "{reason:?}");
        }
        assert_eq!(
            fb::RefusalReason::ENUM_VALUES.len(),
            16,
            "a new reason needs a sentence here, not a test edit"
        );

        // Every member of this build, classified the way the contract's value groups say.
        for reason in [
            RefusalReason::GroundNotGenerated,
            RefusalReason::GroundIsAir,
            RefusalReason::SpaceNotGenerated,
            RefusalReason::SpaceBlocked,
            RefusalReason::OutOfReach,
            RefusalReason::PlayerIsDead,
            RefusalReason::SlotEmpty,
            RefusalReason::SlotUnusable,
            RefusalReason::SlotChanged,
            RefusalReason::InventoryBusy,
            RefusalReason::TentAlreadyPlaced,
        ] {
            assert!(
                !reason.is_client_defect(),
                "{reason:?} is the world saying no"
            );
        }
        for reason in [
            RefusalReason::MalformedNoAnchor,
            RefusalReason::MalformedFacing,
            RefusalReason::MalformedSlot,
            RefusalReason::MalformedKind,
        ] {
            assert!(
                reason.is_client_defect(),
                "{reason:?} is this build's own bug"
            );
        }
        // Neither group, and told to nobody: there is no sentence to write for a code this
        // build cannot read that would not be a guess.
        assert!(!RefusalReason::Unknown.is_client_defect());
    }

    /// Every V3 enum's zero member is `Unknown`, and every member's value is pinned.
    ///
    /// The zero is the load-bearing one: FlatBuffers decodes an absent scalar as zero,
    /// so vitals with no life state, or a mob with no kind, must read as something the
    /// decoder refuses rather than as a real state. Renumbering any of the rest is a
    /// wire break that compiles perfectly on both sides.
    #[test]
    fn the_v3_enums_fail_closed_on_zero() {
        assert_eq!(fb::LifeState::Unknown.0, 0);
        assert_eq!(fb::LifeState::Alive.0, 1);
        assert_eq!(fb::LifeState::Dead.0, 2);

        assert_eq!(fb::MobKind::Unknown.0, 0);
        assert_eq!(fb::MobKind::Draugr.0, 1);

        assert_eq!(fb::MobAction::Unknown.0, 0);
        assert_eq!(fb::MobAction::Idle.0, 1);
        assert_eq!(fb::MobAction::Chase.0, 2);
        assert_eq!(fb::MobAction::Windup.0, 3);
        assert_eq!(fb::MobAction::Recovery.0, 4);
    }

    /// The same guarantee for V4's vocabulary, and the zero carries more weight here:
    /// all three of these enums ride on client → server messages, so the absent-field
    /// zero is the value an old or hostile peer produces for free. A `RecipeID` that
    /// read as a real recipe when omitted would be a craft nobody asked for.
    #[test]
    fn the_v4_enums_fail_closed_on_zero() {
        assert_eq!(fb::RecipeID::Unknown.0, 0);
        assert_eq!(fb::RecipeID::Forge.0, 1);
        assert_eq!(fb::RecipeID::IronSword.0, 2);
        assert_eq!(fb::RecipeID::SharpeningStone.0, 3);
        assert_eq!(fb::RecipeID::Tent.0, 4);

        assert_eq!(fb::StructureKind::Unknown.0, 0);
        assert_eq!(fb::StructureKind::Tent.0, 1);
        assert_eq!(fb::StructureKind::Forge.0, 2);

        assert_eq!(fb::Facing::Unknown.0, 0);
        assert_eq!(fb::Facing::North.0, 1);
        assert_eq!(fb::Facing::East.0, 2);
        assert_eq!(fb::Facing::South.0, 3);
        assert_eq!(fb::Facing::West.0, 4);
    }

    /// V6's three new members sit where they were appended, and what they were appended
    /// after has not moved.
    ///
    /// **The number is the contract and nothing else about a member is.** A rename is a
    /// compile error on both sides and is fixed in an afternoon; a renumbering compiles
    /// perfectly everywhere and draws a vargr where the server said draugr, in every
    /// build already shipped. Pinned as integers for that reason, on both sides — the Go
    /// half is `TestV6AppendsWithoutMovingWhatCameBefore`.
    #[test]
    fn the_v6_enums_append_without_moving_what_came_before() {
        // Appended after Draugr = 1.
        assert_eq!(fb::MobKind::Draugr.0, 1);
        assert_eq!(fb::MobKind::Vargr.0, 2);

        // Appended after Forge = 2.
        assert_eq!(fb::StructureKind::Forge.0, 2);
        assert_eq!(fb::StructureKind::Campfire.0, 3);

        // Appended after Tent = 4.
        assert_eq!(fb::RecipeID::Tent.0, 4);
        assert_eq!(fb::RecipeID::Campfire.0, 5);
        assert_eq!(fb::RecipeID::LeatherPatch.0, 6);
    }

    /// V7's members sit where they were appended, and the enums they were appended to
    /// did not move.
    ///
    /// The value is an integer on the wire: a renumbered `RejectReason` relabels every
    /// refusal already written to a log, and a renumbered `HairModel` puts a different
    /// head on every character already stored. `TestV7AppendsWithoutMovingWhatCameBefore`
    /// is the server's half.
    #[test]
    fn the_v7_enums_append_without_moving_what_came_before() {
        // The four RejectReason had before V7, restated so a renumbering fails here.
        assert_eq!(fb::RejectReason::PROTOCOL_MISMATCH.0, 0);
        assert_eq!(fb::RejectReason::SERVER_FULL.0, 1);
        assert_eq!(fb::RejectReason::BAD_REQUEST.0, 2);
        assert_eq!(fb::RejectReason::ALREADY_CONNECTED.0, 3);
        // Appended after ALREADY_CONNECTED = 3.
        assert_eq!(fb::RejectReason::CHARACTER_NAME_TAKEN.0, 4);
        assert_eq!(fb::RejectReason::CHARACTER_NAME_REFUSED.0, 5);
        assert_eq!(fb::RejectReason::CHARACTER_LIMIT_REACHED.0, 6);
        assert_eq!(
            fb::RejectReason::ENUM_VALUES.len(),
            7,
            "a new refusal needs a decision, not a test edit"
        );

        // New in V7. The zero member is the one that matters: an appearance with no
        // hair model must fail closed rather than read as a head somebody chose.
        assert_eq!(fb::HairModel::Unknown.0, 0);
        assert_eq!(fb::HairModel::Shaved.0, 1);
        assert_eq!(fb::HairModel::Cropped.0, 2);
        assert_eq!(fb::HairModel::Braided.0, 3);
        assert_eq!(fb::HairModel::Loose.0, 4);
        assert_eq!(fb::HairModel::Topknot.0, 5);
        assert_eq!(
            fb::HairModel::ENUM_VALUES.len(),
            6,
            "a new hair model needs a decision, not a test edit"
        );
    }

    /// Every hair model the contract declares is one this client offers.
    ///
    /// The other half of the test above, and the half that catches the *likely*
    /// mistake. That one pins the numbering, so a member appended to
    /// `schemas/handshake.fbs` fails it with "a new hair model needs a decision, not a
    /// test edit" — and the decision it asks for is a number, which is satisfied by
    /// bumping the count. Nothing there points at [`HairModel::ALL`], the hand-written
    /// list the character screen builds its choice from, so a sixth model could arrive
    /// on the wire, decode, render, and still be one no player could pick.
    ///
    /// This reads `ENUM_VALUES` rather than restating it: it is flatc's output, so what
    /// it holds is what the schema says and not what anybody typed here.
    #[test]
    fn every_wire_hair_model_but_unknown_is_one_a_player_can_pick() {
        let declared: Vec<fb::HairModel> = fb::HairModel::ENUM_VALUES
            .iter()
            .copied()
            // `Unknown` is the absent-field case and has no variant by construction —
            // see [`HairModel`]. It is the one member that must *not* be offered.
            .filter(|value| *value != fb::HairModel::Unknown)
            .collect();

        assert_eq!(
            HairModel::ALL.len(),
            declared.len(),
            "the contract declares {} models a player may wear and `HairModel::ALL` \
             offers {}",
            declared.len(),
            HairModel::ALL.len()
        );

        for value in declared {
            let model = HairModel::from_wire(value)
                .unwrap_or_else(|| panic!("{value:?} is on the wire and does not decode"));
            assert!(
                HairModel::ALL.contains(&model),
                "{model:?} decodes and is not offered on the character screen"
            );
        }
    }

    /// V7 gave every player an appearance and put none of it in `EntityState`.
    ///
    /// `EntityState` is a struct, so its size is the stride of the entity array in
    /// every snapshot — the most frequently sent payload in the game. This is what
    /// catches somebody quietly adding a field later, which a FlatBuffers struct can
    /// never take back. `TestEntityStateIsStillFortyBytesOnTheWire` is the other side's
    /// half, measured from an encoded frame because Go has no `size_of` for one.
    #[test]
    fn entity_state_is_still_forty_bytes_on_the_wire() {
        assert_eq!(
            size_of::<fb::EntityState>(),
            40,
            "appearance belongs in PlayerAppearance, not in the struct every tick carries"
        );
    }

    /// This build speaks V6's two drawable members and draws them both.
    ///
    /// **The inverse of what this test asserted until legacy PR 172, and the change is the point.**
    /// The contract reserved the names in legacy PR 166 and nothing on this side could draw them,
    /// so refusing was the honest answer: a default shape would have put a creature in the
    /// world the server never described. The renderer arrived with this commit, so the
    /// decoder accepts what the renderer can draw — and `session.rs` turns every decode
    /// error into a protocol failure, which is why the two halves are one change. A
    /// snapshot carrying a vargr used to end the connection.
    ///
    /// `RecipeID::Campfire` is absent from *this* test rather than from this client, and
    /// the distinction is the whole of #113. Nothing on the wire ever *sends* a recipe id
    /// here, so a recipe member is not something this decoder accepts or refuses — it is
    /// an outbound vocabulary, swept against the contract in
    /// [`a_craft_request_carries_one_recipe_member_and_a_tick`] instead. What that
    /// vocabulary was missing until #113 was the campfire and the leather patch, which
    /// left the panel unable to originate a craft for either.
    #[test]
    fn the_kinds_v6_reserved_are_the_kinds_this_build_now_draws() {
        assert_eq!(MobKind::from_wire(fb::MobKind::Vargr), Some(MobKind::Vargr));
        assert_eq!(
            StructureKind::from_wire(fb::StructureKind::Campfire),
            Some(StructureKind::Campfire)
        );
    }

    /// Accepting the reserved members did not open the door behind them.
    ///
    /// `Unknown` is how the contract fails closed and a member past the end is a server
    /// one contract ahead; both stay a decode error, because the alternative is drawing a
    /// creature this build has never heard of as something it is not.
    #[test]
    fn a_kind_this_build_has_never_heard_of_is_still_refused() {
        assert_eq!(MobKind::from_wire(fb::MobKind::Unknown), None);
        assert_eq!(MobKind::from_wire(fb::MobKind(3)), None);
        assert_eq!(MobKind::from_wire(fb::MobKind(200)), None);

        assert_eq!(StructureKind::from_wire(fb::StructureKind::Unknown), None);
        assert_eq!(StructureKind::from_wire(fb::StructureKind(4)), None);
        assert_eq!(StructureKind::from_wire(fb::StructureKind(200)), None);
    }

    /// A welcome that declares no clock is a world where time does not pass, and that
    /// is what every server in this repository announces today.
    ///
    /// Three absent scalars decode as three zeros, which is exactly what a pre-V6 server
    /// put on the wire by not having the fields at all — so this test is also the one
    /// that says an older peer stays readable.
    #[test]
    fn a_welcome_with_no_clock_is_a_world_where_time_does_not_pass() {
        let Ok(Message::Welcome(params)) = decode_welcome(&WelcomeWire::default()) else {
            panic!("a default welcome is accepted");
        };

        assert_eq!(params.clock, WorldClock::default());
        assert!(!params.clock.declared());
    }

    /// `WorldClock::default()` is the absence of a clock, which is what lets every
    /// session fixture in this crate write `clock: Default::default()` and mean it.
    ///
    /// Pinned rather than assumed: a `Default` that ever gained a non-zero day length
    /// would quietly give all of them a clock, and every one of those fixtures would
    /// start exercising a path it was never written for.
    #[test]
    fn the_default_clock_is_no_clock() {
        assert_eq!(WorldClock::default().day_length_ticks, 0);
        assert!(!WorldClock::default().declared());
    }

    /// A declared clock arrives whole, boundaries included.
    #[test]
    fn a_declared_clock_survives_the_welcome() {
        let clock = WorldClock {
            day_length_ticks: 24_000,
            night_start_ticks: 14_400,
            night_end_ticks: 21_600,
        };
        let Ok(Message::Welcome(params)) = decode_welcome(&WelcomeWire {
            clock,
            ..WelcomeWire::default()
        }) else {
            panic!("a well-ordered clock is accepted");
        };

        assert_eq!(params.clock, clock);
        assert!(params.clock.declared());
    }

    /// A clock whose night is out of order is refused, never repaired.
    ///
    /// Four ways to be wrong and one boundary that must still be accepted. The last case
    /// is the one an over-eager bound would break: a night that begins on tick 1 and runs
    /// to the final tick of the day is legal, because `night_end_ticks` is compared with
    /// `<=` against the day length while `tick_of_day` is compared with `<`.
    #[test]
    fn a_clock_whose_night_is_out_of_order_is_a_protocol_error() {
        for (day_length_ticks, night_start_ticks, night_end_ticks, accepted) in [
            // Night cannot begin at tick 0: zero is how the boundaries say "no clock",
            // and a declared clock may not use it to mean midnight.
            (24_000, 0, 21_600, false),
            // Ends before it begins.
            (24_000, 21_600, 14_400, false),
            // A night of no length is not a night.
            (24_000, 14_400, 14_400, false),
            // Ends after the day it is inside.
            (24_000, 14_400, 24_001, false),
            // The boundary that must hold: the whole day but its first tick.
            (24_000, 1, 24_000, true),
        ] {
            let clock = WorldClock {
                day_length_ticks,
                night_start_ticks,
                night_end_ticks,
            };
            let decoded = decode_welcome(&WelcomeWire {
                clock,
                ..WelcomeWire::default()
            });

            if accepted {
                let Ok(Message::Welcome(params)) = decoded else {
                    panic!(
                        "night {night_start_ticks}..{night_end_ticks} of {day_length_ticks} is legal"
                    );
                };
                assert_eq!(params.clock, clock);
            } else {
                assert_eq!(
                    decoded,
                    Err(DecodeError::WorldClock {
                        day_length: day_length_ticks,
                        night_start: night_start_ticks,
                        night_end: night_end_ticks,
                    }),
                    "night {night_start_ticks}..{night_end_ticks} of {day_length_ticks}"
                );
            }
        }
    }

    /// The boundaries of a clock that was never declared are not examined at all.
    ///
    /// Nonsense in those two fields is not an error when the day length is zero, and it
    /// must not be: the ordering rule would otherwise turn the legal pre-V6 shape into a
    /// refusal the moment a server left a stale value in either scalar.
    #[test]
    fn boundaries_are_not_read_when_no_clock_is_declared() {
        let Ok(Message::Welcome(params)) = decode_welcome(&WelcomeWire {
            clock: WorldClock {
                day_length_ticks: 0,
                night_start_ticks: 9_000,
                night_end_ticks: 3,
            },
            ..WelcomeWire::default()
        }) else {
            panic!("an undeclared clock is accepted whatever its boundaries say");
        };

        assert!(!params.clock.declared());
    }

    /// The tick of day rides in the snapshot and arrives unchanged, the last tick of a
    /// day included — the value an off-by-one loses, and a legal one: the contract's
    /// bound is `tick_of_day < day_length_ticks`.
    ///
    /// Nothing here checks that bound, and that is the design: this layer decodes one
    /// frame and the day length arrived in another. `net::handshake` owns the check.
    #[test]
    fn a_snapshot_carries_the_tick_of_day() {
        for tick_of_day in [0_u32, 1, 14_400, 23_999] {
            let frame = server_side::encode_entity_snapshot_at_tick_of_day(7, tick_of_day);
            let Ok(Message::Snapshot(snapshot)) = decode(&frame) else {
                panic!("a snapshot with a tick of day is a snapshot");
            };

            assert_eq!(snapshot.tick_of_day, tick_of_day);
            // The field it was appended after, read in the same breath: an appended
            // scalar that displaced an existing one would satisfy the line above and
            // still have broken the contract.
            assert_eq!(snapshot.server_tick, 7);
        }
    }

    #[test]
    fn a_client_hello_round_trips_and_is_recognised_as_client_only() {
        // Direction is a protocol rule: the client's own message, arriving from a
        // server, is a protocol error rather than something to handle.
        let frame = encode_client_hello("thora", None, None);
        assert_eq!(decode(&frame), Ok(Message::ClientOnly("ClientHello")));
    }

    #[test]
    fn a_client_hello_carries_the_current_protocol_version() {
        let frame = encode_client_hello("thora", None, None);
        let envelope = fb::root_as_envelope(&frame).expect("our own encoder produces valid bytes");
        let hello = envelope
            .payload_as_client_hello()
            .expect("the payload is a ClientHello");

        assert_eq!(hello.protocol_version(), fb::ProtocolVersion::Current);
        assert_eq!(hello.player_name(), Some("thora"));
    }

    // -----------------------------------------------------------------------
    // Protocol V7 — the session ticket, the character phase, and the face
    // -----------------------------------------------------------------------

    /// A legal appearance, matching [`AppearanceWire::default`] field for field. Every
    /// value is distinct so a transposition shows up as a wrong colour rather than an
    /// equal one.
    ///
    /// Built through [`Appearance::new`] rather than as a struct literal, which these
    /// tests could still write: the constructor is the only door every other module has,
    /// so a fixture that went around it would be testing a value no caller can make.
    fn an_appearance() -> Appearance {
        an_appearance_wearing(HairModel::Braided)
    }

    /// The same face under a different hair model.
    fn an_appearance_wearing(hair_model: HairModel) -> Appearance {
        Appearance::new(
            0x00E3_C4A0,
            0x004A_5D3B,
            0x002B_2118,
            0x0055_3311,
            hair_model,
            0x00B0_7A32,
        )
        .expect("every colour is inside the contract's range")
    }

    /// Reads the ticket off a hello this module encoded.
    fn hello_ticket(frame: &[u8]) -> Option<Vec<u8>> {
        let envelope = fb::root_as_envelope(frame).expect("our own encoder produces valid bytes");
        envelope
            .payload_as_client_hello()
            .expect("the payload is a ClientHello")
            .session_ticket()
            .map(|ticket| ticket.bytes().to_vec())
    }

    /// Absent, not empty, and for the reason the token is: the contract reads both as
    /// "nothing presented", and absent is the one that does not put a zero-length
    /// vector on the wire to say so. This is what `net/session.rs` sends today.
    #[test]
    fn a_client_with_no_account_presents_no_ticket_at_all() {
        assert_eq!(
            hello_ticket(&encode_client_hello("thora", None, None)),
            None
        );
    }

    #[test]
    fn a_client_with_an_account_presents_the_ticket_it_holds() {
        let ticket = SessionTicket::from_bytes([0x5c; SESSION_TICKET_LEN]);
        let carried = hello_ticket(&encode_client_hello("thora", None, Some(ticket)))
            .expect("a presented ticket reaches the wire");
        assert_eq!(carried, ticket.as_bytes());
        assert_eq!(carried.len(), SESSION_TICKET_LEN);
    }

    /// The two fields are apart on the wire and must not be confused for one another: a
    /// V6 peer writes only the token, a V7 peer only the ticket, and a peer in between
    /// writes both.
    #[test]
    fn a_hello_carries_the_token_and_the_ticket_apart() {
        let token = PlayerToken::from_bytes([0xA1; PLAYER_TOKEN_LEN]);
        let ticket = SessionTicket::from_bytes([0xB2; SESSION_TICKET_LEN]);
        let frame = encode_client_hello("thora", Some(token), Some(ticket));

        assert_eq!(hello_token(&frame).as_deref(), Some(&token.as_bytes()[..]));
        assert_eq!(
            hello_ticket(&frame).as_deref(),
            Some(&ticket.as_bytes()[..])
        );
    }

    /// The redaction is a property of the type, exactly as it is for [`PlayerToken`].
    /// A signature says who *issued* a ticket, not who is holding one, so a ticket in a
    /// log line is as good as the account it names.
    #[test]
    fn a_ticket_is_never_printed_by_a_debug_that_holds_one() {
        let ticket = SessionTicket::from_bytes([0xAB; SESSION_TICKET_LEN]);
        let printed = format!("{ticket:?}");

        assert_eq!(printed, "SessionTicket(<redacted>)");
        assert!(!printed.contains("171"), "the bytes reached the output");
        assert!(
            !printed.contains("ab"),
            "the bytes reached the output as hex"
        );
    }

    /// The whole of a character list, in the order the server sent it.
    #[test]
    fn a_character_list_decodes_into_its_characters() {
        let wire = [
            CharacterSummaryWire::default(),
            CharacterSummaryWire {
                character_id: 7,
                name: Some("Sigrún".to_owned()),
                appearance: Some(AppearanceWire {
                    hair_model: fb::HairModel::Shaved,
                    ..AppearanceWire::default()
                }),
            },
        ];

        let decoded = decode(&encode_server_character_list(Some(&wire), 5));

        assert_eq!(
            decoded,
            Ok(Message::CharacterList(CharacterList {
                characters: vec![
                    CharacterSummary {
                        character_id: 900,
                        name: "Eivor".to_owned(),
                        appearance: an_appearance(),
                    },
                    CharacterSummary {
                        character_id: 7,
                        name: "Sigrún".to_owned(),
                        appearance: an_appearance_wearing(HairModel::Shaved),
                    },
                ],
                max_characters: 5,
            }))
        );
    }

    /// An account with no characters here is a legal, expected answer and not a
    /// refusal: it says the only way forward is a creation. An absent vector and an
    /// empty one say the same thing, so both must decode the same way.
    #[test]
    fn an_account_with_no_characters_here_is_not_a_refusal() {
        let empty = decode(&encode_server_character_list(Some(&[]), 3));
        let absent = decode(&encode_server_character_list(None, 3));

        assert_eq!(
            empty,
            Ok(Message::CharacterList(CharacterList {
                characters: Vec::new(),
                max_characters: 3,
            }))
        );
        assert_eq!(absent, empty);
    }

    /// A name that is not UTF-8 is refused before the accessor that would read it runs.
    ///
    /// The generated `name()` accessor is `from_utf8_unchecked`, so the verifier is the
    /// only thing between a malicious frame and undefined behaviour. It holds — [`decode`]
    /// goes through `root_as_envelope`, whose verifier runs `core::str::from_utf8` on every
    /// string it visits — but it holds *invisibly*, by library behaviour a call site cannot
    /// show, guarded only by the "never `root_as_envelope_unchecked`" convention and the
    /// version pinned in `Cargo.lock`. #117's review asked for the property to be pinned
    /// rather than left to those two, and it was right to: both are conventions, and a
    /// convention is what a regression walks through.
    ///
    /// **The bytes are patched into a finished frame rather than built through
    /// `from_utf8_unchecked`.** `client/Cargo.toml` records that hand-written client code
    /// contains no `unsafe`, and there is none today — writing the first one to test a
    /// safety property would spend more than the test is worth. Patching is the more
    /// faithful fixture anyway: it is exactly the bytes a hostile peer puts on the wire,
    /// and the replacement is the same length as what it replaces, so no offset moves.
    #[test]
    fn a_character_name_that_is_not_utf8_is_refused_before_the_accessor_runs() {
        // Distinctive enough to appear once in a frame, and checked below rather than
        // assumed. 0xC3 opens a two-byte sequence and 0x28 is not a continuation byte.
        const NAME: &[u8] = b"Qxvz";
        const NOT_UTF8: &[u8] = &[0xC3, 0x28];

        let mut frame = encode_server_character_list(
            Some(&[CharacterSummaryWire {
                character_id: 9,
                name: Some(String::from_utf8(NAME.to_vec()).expect("the fixture name is ascii")),
                ..CharacterSummaryWire::default()
            }]),
            3,
        );

        // The frame this patches is a good one, which is what makes the refusal below a
        // statement about the bytes rather than about the fixture.
        assert!(
            decode(&frame).is_ok(),
            "the unpatched fixture is not a decodable frame"
        );

        let occurrences = frame.windows(NAME.len()).filter(|w| *w == NAME).count();
        assert_eq!(occurrences, 1, "the fixture name is not uniquely locatable");
        let at = frame
            .windows(NAME.len())
            .position(|window| window == NAME)
            .expect("the name is in the frame it was encoded into");
        frame[at..at + NOT_UTF8.len()].copy_from_slice(NOT_UTF8);

        // The reason is pinned, not just the refusal. Patching bytes into a finished
        // buffer could in principle break something else and be refused for a reason
        // that has nothing to do with UTF-8 — which would leave this test passing while
        // the property it exists for went unchecked. The verifier names the field:
        // `Utf8 error for string in 136..140 ... while verifying table field \`name\``.
        let refusal = decode(&frame);
        let Err(DecodeError::Malformed(reason)) = &refusal else {
            panic!("invalid UTF-8 in a name was not refused: {refusal:?}");
        };
        assert!(
            reason.contains("Utf8") && reason.contains("name"),
            "the frame was refused for something other than the name's encoding: {reason}"
        );
    }

    #[test]
    fn a_character_list_that_breaks_its_own_invariants_is_a_protocol_error() {
        let named = |id: u64, name: Option<&str>| CharacterSummaryWire {
            character_id: id,
            name: name.map(str::to_owned),
            ..CharacterSummaryWire::default()
        };

        for (case, frame, want) in [
            (
                "the reserved id 0",
                encode_server_character_list(Some(&[named(0, Some("Eivor"))]), 3),
                DecodeError::CharacterWithoutIdentity,
            ),
            (
                "no name at all",
                encode_server_character_list(Some(&[named(9, None)]), 3),
                DecodeError::CharacterWithoutName(9),
            ),
            (
                "an empty name",
                encode_server_character_list(Some(&[named(9, Some(""))]), 3),
                DecodeError::CharacterWithoutName(9),
            ),
            (
                "one id twice",
                encode_server_character_list(
                    Some(&[named(9, Some("Eivor")), named(9, Some("Sigrún"))]),
                    3,
                ),
                DecodeError::DuplicateCharacter(9),
            ),
            (
                "a limit of none",
                encode_server_character_list(Some(&[]), 0),
                DecodeError::CharacterLimit { listed: 0, max: 0 },
            ),
            (
                "more characters than the limit allows",
                encode_server_character_list(
                    Some(&[named(9, Some("Eivor")), named(10, Some("Sigrún"))]),
                    1,
                ),
                DecodeError::CharacterLimit { listed: 2, max: 1 },
            ),
        ] {
            assert_eq!(decode(&frame), Err(want), "{case}");
        }
    }

    /// A summary with no appearance is refused rather than filled in with
    /// [`PLACEHOLDER_APPEARANCE`]: the placeholder answers "the message has not
    /// arrived", and this one arrived.
    #[test]
    fn a_character_with_no_appearance_is_a_protocol_error() {
        let frame = encode_server_character_list(
            Some(&[CharacterSummaryWire {
                appearance: None,
                ..CharacterSummaryWire::default()
            }]),
            3,
        );

        assert_eq!(
            decode(&frame),
            Err(DecodeError::MissingAppearance {
                at: "CharacterSummary"
            })
        );
    }

    #[test]
    fn a_player_appearance_decodes_into_its_entity_and_its_face() {
        let decoded = decode(&encode_player_appearance(
            4242,
            Some(AppearanceWire::default()),
        ));

        assert_eq!(
            decoded,
            Ok(Message::PlayerAppearance(PlayerAppearance {
                entity_id: 4242,
                appearance: an_appearance(),
            }))
        );
    }

    /// **An appearance for an entity this client has never seen is not an error**, and
    /// `schemas/player.fbs` says why: the appearance stream and the snapshot stream are
    /// not ordered against each other, so either can arrive first.
    ///
    /// The decoder is the only place that could get this wrong, and the way it would is
    /// by growing a check against state it does not have. There is no snapshot in this
    /// test at all — which is exactly the situation the rule is about.
    #[test]
    fn an_appearance_for_an_entity_nobody_has_seen_still_decodes() {
        let decoded = decode(&encode_player_appearance(
            u64::MAX,
            Some(AppearanceWire::default()),
        ));

        assert!(
            matches!(decoded, Ok(Message::PlayerAppearance(_))),
            "an unseen entity is the ordinary case, not a refusal; got {decoded:?}"
        );
    }

    #[test]
    fn a_player_appearance_that_breaks_its_invariants_is_a_protocol_error() {
        assert_eq!(
            decode(&encode_player_appearance(
                0,
                Some(AppearanceWire::default())
            )),
            Err(DecodeError::AppearanceWithoutEntity)
        );
        assert_eq!(
            decode(&encode_player_appearance(1, None)),
            Err(DecodeError::MissingAppearance {
                at: "PlayerAppearance"
            })
        );
    }

    /// Every colour's reserved top eight bits are checked, and each one is named.
    ///
    /// Refused rather than masked: a set high byte means the peer is encoding something
    /// this build does not know about, and masking would draw a colour nobody chose
    /// while hiding the disagreement. The loop covers all five so a colour cannot be
    /// added with the check forgotten.
    #[test]
    fn a_colour_with_its_reserved_byte_set_is_a_protocol_error() {
        for field in [
            "skin_color",
            "shirt_color",
            "trousers_color",
            "shoes_color",
            "hair_color",
        ] {
            let mut wire = AppearanceWire::default();
            // Distinct high bytes, so a check that read the wrong field would report a
            // value that does not match the one it was handed.
            match field {
                "skin_color" => wire.skin_color |= 0xFF00_0000,
                "shirt_color" => wire.shirt_color |= 0x0100_0000,
                "trousers_color" => wire.trousers_color |= 0x8000_0000,
                "shoes_color" => wire.shoes_color |= 0xAB00_0000,
                _ => wire.hair_color |= 0x7F00_0000,
            }
            match decode(&encode_player_appearance(1, Some(wire))) {
                Err(DecodeError::AppearanceColorReserved { field: got, .. }) => {
                    assert_eq!(got, field)
                }
                other => panic!("{field} was accepted or misreported: {other:?}"),
            }
        }
    }

    /// The same rule on the way *out*: an appearance carrying a reserved byte cannot be
    /// built, so no encoder has to check for one.
    ///
    /// **This is why the fields are private.** The character screen composes a colour
    /// from a palette and the decoder composes one from the wire, and both reach the
    /// same constructor — a struct literal would be a second way in, checked nowhere.
    /// The refusal names the field and carries the value, because the one caller that
    /// can hit it is a screen that has to say which choice it could not take.
    #[test]
    fn an_appearance_carrying_a_reserved_byte_cannot_be_built() {
        let legal = an_appearance();
        let colours = [
            ("skin_color", legal.skin_color()),
            ("shirt_color", legal.shirt_color()),
            ("trousers_color", legal.trousers_color()),
            ("shoes_color", legal.shoes_color()),
            ("hair_color", legal.hair_color()),
        ];

        for (index, (field, value)) in colours.iter().enumerate() {
            let mut given = colours.map(|(_, colour)| colour);
            given[index] = value | 0xAB00_0000;
            let refused = Appearance::new(
                given[0],
                given[1],
                given[2],
                given[3],
                HairModel::Braided,
                given[4],
            )
            .expect_err("a reserved byte is set");

            assert_eq!(refused.field, *field);
            assert_eq!(refused.value, given[index], "the value the caller offered");
        }

        // And the legal one keeps every colour it was given, in the field it was given
        // for: a constructor that validated correctly and transposed two arguments would
        // pass every check above.
        assert_eq!(legal.skin_color(), 0x00E3_C4A0);
        assert_eq!(legal.shirt_color(), 0x004A_5D3B);
        assert_eq!(legal.trousers_color(), 0x002B_2118);
        assert_eq!(legal.shoes_color(), 0x0055_3311);
        assert_eq!(legal.hair_model(), HairModel::Braided);
        assert_eq!(legal.hair_color(), 0x00B0_7A32);
    }

    /// `Unknown` is the absent-field case and a member past the end is a server one
    /// contract ahead. Both are refused, because `schemas/common.fbs` says the client
    /// renders what the player chose and invents no default.
    #[test]
    fn a_hair_model_this_build_cannot_name_is_a_protocol_error() {
        for value in [fb::HairModel::Unknown, fb::HairModel(200)] {
            let wire = AppearanceWire {
                hair_model: value,
                ..AppearanceWire::default()
            };
            assert_eq!(
                decode(&encode_player_appearance(1, Some(wire))),
                Err(DecodeError::UnknownHairModel(value.0))
            );
        }
    }

    /// Every declared hair model decodes, unlike [`StructureKind`], and the difference
    /// is deliberate: hair is a property of a player the server already placed, so
    /// refusing one this build has no mesh for would refuse the whole player.
    #[test]
    fn every_declared_hair_model_reaches_this_build() {
        for (wire, want) in [
            (fb::HairModel::Shaved, HairModel::Shaved),
            (fb::HairModel::Cropped, HairModel::Cropped),
            (fb::HairModel::Braided, HairModel::Braided),
            (fb::HairModel::Loose, HairModel::Loose),
            (fb::HairModel::Topknot, HairModel::Topknot),
        ] {
            assert_eq!(HairModel::from_wire(wire), Some(want));
        }
    }

    /// The placeholder is a legal appearance in its own right — it has to be, since it
    /// is what a renderer draws — and it is deliberately not what a missing one decodes
    /// to. The tests above pin the second half; this pins the first.
    #[test]
    fn the_placeholder_is_itself_a_legal_appearance() {
        let wire = AppearanceWire {
            skin_color: PLACEHOLDER_APPEARANCE.skin_color(),
            shirt_color: PLACEHOLDER_APPEARANCE.shirt_color(),
            trousers_color: PLACEHOLDER_APPEARANCE.trousers_color(),
            shoes_color: PLACEHOLDER_APPEARANCE.shoes_color(),
            hair_model: PLACEHOLDER_APPEARANCE.hair_model().wire(),
            hair_color: PLACEHOLDER_APPEARANCE.hair_color(),
        };

        assert_eq!(
            decode(&encode_player_appearance(1, Some(wire))),
            Ok(Message::PlayerAppearance(PlayerAppearance {
                entity_id: 1,
                appearance: PLACEHOLDER_APPEARANCE,
            }))
        );
    }

    /// The client→server half of the character phase: the bytes reach the wire intact,
    /// and receiving one back is a protocol error because direction is a protocol rule.
    #[test]
    fn a_select_character_request_round_trips_and_is_recognised_as_client_only() {
        for character_id in [900_u64, 0, u64::MAX] {
            let frame = encode_select_character_request(character_id);

            let envelope = fb::root_as_envelope(&frame).expect("our own encoder is valid");
            let payload = envelope
                .payload_as_select_character_request()
                .expect("the payload is a SelectCharacterRequest");
            assert_eq!(payload.character_id(), character_id);

            assert_eq!(
                decode(&frame),
                Ok(Message::ClientOnly("SelectCharacterRequest"))
            );
        }
    }

    #[test]
    fn a_create_character_request_round_trips_and_is_recognised_as_client_only() {
        // The empty name included: what names a server accepts is the server's rule,
        // answered with CHARACTER_NAME_REFUSED, so this encoder holds no opinion.
        for name in ["Sigrún", ""] {
            let request = CreateCharacterRequest {
                name: name.to_owned(),
                appearance: an_appearance(),
            };
            let frame = encode_create_character_request(&request);

            let envelope = fb::root_as_envelope(&frame).expect("our own encoder is valid");
            let payload = envelope
                .payload_as_create_character_request()
                .expect("the payload is a CreateCharacterRequest");
            assert_eq!(payload.name(), Some(name));
            let appearance = payload.appearance().expect("the request carries a face");
            assert_eq!(appearance.skin_color(), an_appearance().skin_color());
            assert_eq!(appearance.hair_model(), fb::HairModel::Braided);

            assert_eq!(
                decode(&frame),
                Ok(Message::ClientOnly("CreateCharacterRequest"))
            );
        }
    }

    /// Decode is total over damage anywhere in V7's two new server payloads: every
    /// truncation and every flipped byte either decodes or errors, and none panics.
    #[test]
    fn every_truncation_and_corruption_of_a_v7_payload_survives_decoding() {
        let frames = [
            encode_server_character_list(Some(&[CharacterSummaryWire::default()]), 3),
            encode_player_appearance(1, Some(AppearanceWire::default())),
        ];

        for frame in frames {
            for cut in 0..frame.len() {
                let _ = decode(&frame[..cut]);
            }
            for index in 0..frame.len() {
                let mut damaged = frame.clone();
                damaged[index] ^= 0xFF;
                let _ = decode(&damaged);
            }
        }
    }

    /// Reads the token off a hello this module encoded.
    fn hello_token(frame: &[u8]) -> Option<Vec<u8>> {
        let envelope = fb::root_as_envelope(frame).expect("our own encoder produces valid bytes");
        envelope
            .payload_as_client_hello()
            .expect("the payload is a ClientHello")
            .player_token()
            .map(|token| token.bytes().to_vec())
    }

    #[test]
    fn a_first_connection_presents_no_token_at_all() {
        // Absent, not empty: the contract reads both as a first connection, and
        // absent is the one that does not put a zero-length vector on the wire.
        assert_eq!(hello_token(&encode_client_hello("thora", None, None)), None);
    }

    #[test]
    fn a_returning_client_presents_the_token_it_holds() {
        let token = PlayerToken::from_bytes([0x5a; PLAYER_TOKEN_LEN]);
        let carried = hello_token(&encode_client_hello("thora", Some(token), None))
            .expect("a presented token reaches the wire");

        assert_eq!(carried.len(), PLAYER_TOKEN_LEN);
        assert_eq!(carried, token.as_bytes().to_vec());
    }

    #[test]
    fn a_valid_welcome_decodes_into_session_params() {
        let wire = WelcomeWire {
            entity_id: 42,
            spawn: Some([0.5, 80.0, -3.25]),
            world_seed: -900,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 36,
            hotbar_slots: 9,
            player_token: Some(vec![7; PLAYER_TOKEN_LEN]),
            clock: WorldClock {
                day_length_ticks: 24_000,
                night_start_ticks: 14_400,
                night_end_ticks: 21_600,
            },
        };

        assert_eq!(
            decode_welcome(&wire),
            Ok(Message::Welcome(SessionParams {
                entity_id: 42,
                spawn: [0.5, 80.0, -3.25],
                world_seed: -900,
                tick_rate: 20,
                chunk_size: 32,
                view_distance: 8,
                inventory_slots: 36,
                hotbar_slots: 9,
                player_token: PlayerToken::from_bytes([7; PLAYER_TOKEN_LEN]),
                clock: WorldClock {
                    day_length_ticks: 24_000,
                    night_start_ticks: 14_400,
                    night_end_ticks: 21_600,
                },
            }))
        );
    }

    #[test]
    fn a_welcome_that_numbers_this_session_zero_is_a_protocol_error() {
        // Zero is the reserved id everywhere in this contract. It matters most here:
        // a structure may carry owner 0 to mean "offline", and whose-camp-is-whose is
        // decided by comparing that against this number.
        assert_eq!(
            decode_welcome(&WelcomeWire {
                entity_id: 0,
                ..WelcomeWire::default()
            }),
            Err(DecodeError::WelcomeEntityId)
        );
    }

    #[test]
    fn a_welcome_without_a_token_is_a_protocol_error() {
        // Refused the way a zero tick rate is: the contract guarantees a token on
        // every accepted handshake, so its absence is a server that has not
        // settled an identity.
        assert_eq!(
            decode_welcome(&WelcomeWire {
                player_token: None,
                ..WelcomeWire::default()
            }),
            Err(DecodeError::MissingPlayerToken)
        );
    }

    #[test]
    fn a_token_of_the_wrong_length_is_a_protocol_error() {
        // One byte short, one byte long, and empty — all three are lengths a
        // padding or truncating decoder would quietly turn into an identity the
        // server never issued.
        for len in [0, PLAYER_TOKEN_LEN - 1, PLAYER_TOKEN_LEN + 1] {
            assert_eq!(
                decode_welcome(&WelcomeWire {
                    player_token: Some(vec![3; len]),
                    ..WelcomeWire::default()
                }),
                Err(DecodeError::PlayerTokenLength(len)),
                "{len} bytes"
            );
        }
    }

    #[test]
    fn a_refused_token_is_reported_by_length_and_never_by_value() {
        // The decoder's error text is the one place a rejected token could leak
        // into a log, since a refusal is exactly what gets written down.
        let token = vec![0x5a; PLAYER_TOKEN_LEN + 1];
        let err = decode_welcome(&WelcomeWire {
            player_token: Some(token.clone()),
            ..WelcomeWire::default()
        })
        .expect_err("33 bytes is not a token");

        let said = format!("{err} / {err:?}");
        assert!(said.contains("33"), "the length is worth saying: {said}");
        assert!(
            !said.contains("5a") && !said.contains("90"),
            "the bytes are not: {said}"
        );
    }

    #[test]
    fn the_default_wire_welcome_carries_a_legal_token() {
        // Every welcome test that is not about identity leans on this, so it is
        // worth one assertion of its own rather than a hundred silent ones.
        let Ok(Message::Welcome(params)) = decode_welcome(&WelcomeWire::default()) else {
            panic!("the default welcome must decode");
        };
        assert_eq!(params.player_token, PlayerToken::from_bytes(DEFAULT_TOKEN));
    }

    #[test]
    fn a_token_is_never_printed_by_a_debug_that_holds_one() {
        // The redaction is a property of the type, so it holds for every struct
        // that carries one — which is the reason it is a newtype at all.
        let token = PlayerToken::from_bytes([0x5a; PLAYER_TOKEN_LEN]);
        let Ok(Message::Welcome(params)) = decode_welcome(&WelcomeWire {
            player_token: Some(vec![0x5a; PLAYER_TOKEN_LEN]),
            ..WelcomeWire::default()
        }) else {
            panic!("a legal welcome must decode");
        };

        for shown in [format!("{token:?}"), format!("{params:?}")] {
            assert!(shown.contains("redacted"), "{shown}");
            assert!(!shown.contains("90"), "no byte of it, in decimal: {shown}");
            assert!(!shown.contains("5a"), "nor in hex: {shown}");
        }
    }

    #[test]
    fn the_permitted_extremes_are_accepted() {
        let wire = WelcomeWire::at_the_limits();
        let Ok(Message::Welcome(params)) = decode_welcome(&wire) else {
            panic!("the contract's own limits must decode");
        };

        assert_eq!(params.tick_rate, u8::MAX);
        assert_eq!(params.chunk_size, MAX_CHUNK_SIZE);
        assert_eq!(params.view_distance, MAX_VIEW_DISTANCE);
    }

    #[test]
    fn the_smallest_permitted_values_are_accepted() {
        let wire = WelcomeWire {
            tick_rate: 1,
            chunk_size: 1,
            view_distance: 0,
            ..WelcomeWire::default()
        };
        let Ok(Message::Welcome(params)) = decode_welcome(&wire) else {
            panic!("1 Hz, 1-block chunks and a zero radius are all legal");
        };

        assert_eq!(
            (params.tick_rate, params.chunk_size, params.view_distance),
            (1, 1, 0)
        );
    }

    #[test]
    fn a_zero_tick_rate_is_a_protocol_error() {
        // The client sends PlayerInput at tick_rate; a zero here is a division.
        let wire = WelcomeWire {
            tick_rate: 0,
            ..WelcomeWire::default()
        };
        assert_eq!(decode_welcome(&wire), Err(DecodeError::TickRate(0)));
    }

    #[test]
    fn a_zero_chunk_size_is_a_protocol_error() {
        let wire = WelcomeWire {
            chunk_size: 0,
            ..WelcomeWire::default()
        };
        assert_eq!(decode_welcome(&wire), Err(DecodeError::ChunkSize(0)));
    }

    #[test]
    fn zero_inventory_and_hotbar_slot_counts_are_protocol_errors() {
        assert_eq!(
            decode_welcome(&WelcomeWire {
                inventory_slots: 0,
                ..WelcomeWire::default()
            }),
            Err(DecodeError::InventorySlots(0))
        );
        assert_eq!(
            decode_welcome(&WelcomeWire {
                hotbar_slots: 0,
                ..WelcomeWire::default()
            }),
            Err(DecodeError::HotbarSlots(0))
        );
    }

    #[test]
    fn a_hotbar_larger_than_the_inventory_is_a_protocol_error() {
        assert_eq!(
            decode_welcome(&WelcomeWire {
                inventory_slots: 8,
                hotbar_slots: 9,
                ..WelcomeWire::default()
            }),
            Err(DecodeError::HotbarExceedsInventory {
                hotbar: 9,
                inventory: 8,
            })
        );
    }

    #[test]
    fn an_oversized_chunk_size_is_a_protocol_error() {
        let wire = WelcomeWire {
            chunk_size: MAX_CHUNK_SIZE + 1,
            ..WelcomeWire::default()
        };
        assert_eq!(
            decode_welcome(&wire),
            Err(DecodeError::ChunkSize(MAX_CHUNK_SIZE + 1))
        );
    }

    #[test]
    fn a_huge_chunk_size_is_refused_before_anything_is_sized_from_it() {
        // 65535³ voxels is what this asks the client to hold.
        let wire = WelcomeWire {
            chunk_size: u16::MAX,
            ..WelcomeWire::default()
        };
        assert_eq!(decode_welcome(&wire), Err(DecodeError::ChunkSize(u16::MAX)));
    }

    #[test]
    fn an_oversized_view_distance_is_a_protocol_error() {
        // (2 * 255 + 1)³ chunks is the allocation this would authorise.
        let wire = WelcomeWire {
            view_distance: u8::MAX,
            ..WelcomeWire::default()
        };
        assert_eq!(
            decode_welcome(&wire),
            Err(DecodeError::ViewDistance(u8::MAX))
        );
    }

    #[test]
    fn the_first_illegal_view_distance_is_a_protocol_error() {
        let wire = WelcomeWire {
            view_distance: MAX_VIEW_DISTANCE + 1,
            ..WelcomeWire::default()
        };
        assert_eq!(
            decode_welcome(&wire),
            Err(DecodeError::ViewDistance(MAX_VIEW_DISTANCE + 1))
        );
    }

    #[test]
    fn a_nan_spawn_is_a_protocol_error() {
        for axis in 0..3 {
            let mut spawn = [1.0f32; 3];
            spawn[axis] = f32::NAN;
            let wire = WelcomeWire {
                spawn: Some(spawn),
                ..WelcomeWire::default()
            };

            let err = decode_welcome(&wire).expect_err("NaN is not a position");
            assert!(
                matches!(err, DecodeError::NonFiniteSpawn { axis: a, .. } if a == axis),
                "axis {axis} got {err:?}"
            );
        }
    }

    #[test]
    fn an_infinite_spawn_is_a_protocol_error() {
        for value in [f32::INFINITY, f32::NEG_INFINITY] {
            let wire = WelcomeWire {
                spawn: Some([0.0, value, 0.0]),
                ..WelcomeWire::default()
            };

            let err = decode_welcome(&wire).expect_err("infinity is not a position");
            assert!(
                matches!(err, DecodeError::NonFiniteSpawn { axis: 1, .. }),
                "got {err:?}"
            );
        }
    }

    #[test]
    fn a_welcome_without_a_spawn_is_a_protocol_error() {
        let wire = WelcomeWire {
            spawn: None,
            ..WelcomeWire::default()
        };
        assert_eq!(decode_welcome(&wire), Err(DecodeError::MissingSpawn));
    }

    #[test]
    fn a_reject_preserves_its_code_and_detail() {
        let frame = encode_server_reject(
            fb::RejectReason::PROTOCOL_MISMATCH,
            "server speaks protocol 1, client speaks 2",
        );

        assert_eq!(
            decode(&frame),
            Ok(Message::Reject(Reject {
                code: "PROTOCOL_MISMATCH",
                detail: "server speaks protocol 1, client speaks 2".to_owned(),
            }))
        );
    }

    #[test]
    fn every_reject_code_decodes_to_its_schema_name() {
        for (reason, name) in [
            (fb::RejectReason::PROTOCOL_MISMATCH, "PROTOCOL_MISMATCH"),
            (fb::RejectReason::SERVER_FULL, "SERVER_FULL"),
            (fb::RejectReason::BAD_REQUEST, "BAD_REQUEST"),
            (fb::RejectReason::ALREADY_CONNECTED, "ALREADY_CONNECTED"),
        ] {
            let frame = encode_server_reject(reason, "why");
            assert_eq!(
                decode(&frame),
                Ok(Message::Reject(Reject {
                    code: name,
                    detail: "why".to_owned()
                }))
            );
        }
    }

    #[test]
    fn a_reject_without_a_detail_decodes_to_an_empty_one() {
        // The field is optional in the schema, so an absent detail must not be a
        // decode failure — the code alone is still worth showing.
        let mut builder = FlatBufferBuilder::with_capacity(64);
        let reject = fb::ServerReject::create(
            &mut builder,
            &fb::ServerRejectArgs {
                reason: fb::RejectReason::SERVER_FULL,
                detail: None,
            },
        );
        let frame = finish_envelope(builder, fb::Payload::ServerReject, reject.as_union_value());

        assert_eq!(
            decode(&frame),
            Ok(Message::Reject(Reject {
                code: "SERVER_FULL",
                detail: String::new(),
            }))
        );
    }

    #[test]
    fn a_snapshot_decodes_into_its_entities() {
        let entities = [
            EntityStateWire {
                entity_id: 1,
                pos: [0.5, 64.0, -0.5],
                vel: [4.3, 0.0, 0.0],
                yaw: 0.25,
            },
            EntityStateWire {
                entity_id: 4096,
                pos: [-100.0, 44.25, 7.0],
                vel: [0.0, -60.0, 0.0],
                yaw: -3.0,
            },
        ];

        // Order is asserted, not just membership: a struct vector is built back to front,
        // so an encoder that forgot to reverse would produce a valid buffer with the list
        // mirrored — and a client that read it the same way would agree with itself and
        // disagree with the server.
        assert_eq!(
            decode(&encode_entity_snapshot(1234, &entities)),
            Ok(Message::Snapshot(Snapshot {
                server_tick: 1234,
                entities: vec![
                    EntityState {
                        entity_id: 1,
                        pos: [0.5, 64.0, -0.5],
                        vel: [4.3, 0.0, 0.0],
                        yaw: 0.25,
                    },
                    EntityState {
                        entity_id: 4096,
                        pos: [-100.0, 44.25, 7.0],
                        vel: [0.0, -60.0, 0.0],
                        yaw: -3.0,
                    },
                ],
                drops: Vec::new(),
                ..Default::default()
            }))
        );
    }

    #[test]
    fn an_empty_snapshot_is_a_snapshot() {
        // A session that can see nobody is an ordinary state, and the tick is still
        // information. Absent and empty are both read as "no entities" — unlike
        // `ChunkData.runs`, where absence is an error because a chunk with no voxels is not
        // a chunk. FlatBuffers is free to omit an empty vector, so both shapes arrive.
        for (name, frame) in [
            ("an empty vector", encode_entity_snapshot(5, &[])),
            ("no vector at all", encode_bare_entity_snapshot(5)),
        ] {
            assert_eq!(
                decode(&frame),
                Ok(Message::Snapshot(Snapshot {
                    tick_of_day: 0,
                    server_tick: 5,
                    entities: Vec::new(),
                    drops: Vec::new(),
                    mobs: vec![],
                    self_vitals: PlayerVitals::unharmed(),
                    structures: vec![],
                })),
                "{name}"
            );
        }
    }

    #[test]
    fn a_snapshot_decodes_item_drops_in_wire_order() {
        let drops = [
            ItemDropStateWire {
                entity_id: 40,
                pos: [-1.0, 70.5, 3.0],
                item_id: 2,
                count: 7,
            },
            ItemDropStateWire {
                entity_id: 41,
                pos: [8.25, 12.0, -9.5],
                item_id: u16::MAX,
                count: u16::MAX,
            },
        ];

        assert_eq!(
            decode(&encode_entity_snapshot_with_drops(88, &[], &drops)),
            Ok(Message::Snapshot(Snapshot {
                server_tick: 88,
                entities: Vec::new(),
                drops: vec![
                    ItemDropState {
                        entity_id: 40,
                        pos: [-1.0, 70.5, 3.0],
                        item_id: 2,
                        count: 7,
                    },
                    ItemDropState {
                        entity_id: 41,
                        pos: [8.25, 12.0, -9.5],
                        item_id: u16::MAX,
                        count: u16::MAX,
                    },
                ],
                ..Default::default()
            }))
        );
    }

    #[test]
    fn malformed_item_drops_are_protocol_errors() {
        let player = EntityStateWire::at(9, 0.0);
        let mut drop = ItemDropStateWire::item(10, 3);
        drop.pos[1] = f32::NAN;
        assert!(matches!(
            decode(&encode_entity_snapshot_with_drops(1, &[], &[drop])),
            Err(DecodeError::NonFiniteDrop {
                entity_id: 10,
                field: "pos.y",
                ..
            })
        ));

        for (drop, error) in [
            (
                ItemDropStateWire::item(11, 0),
                DecodeError::DropWithoutItem(11),
            ),
            (
                ItemDropStateWire {
                    count: 0,
                    ..ItemDropStateWire::item(12, 4)
                },
                DecodeError::EmptyDrop(12),
            ),
        ] {
            assert_eq!(
                decode(&encode_entity_snapshot_with_drops(1, &[], &[drop])),
                Err(error)
            );
        }

        assert_eq!(
            decode(&encode_entity_snapshot_with_drops(
                1,
                &[player],
                &[ItemDropStateWire::item(9, 2)],
            )),
            Err(DecodeError::PlayerDropEntityConflict(9))
        );
    }

    #[test]
    fn a_non_finite_entity_is_a_protocol_error() {
        // The finite-float invariant in the server-to-client direction. A NaN here would
        // pass every range check, sail through the interpolation and land in a `Transform`,
        // taking every child of it with it — so the decoder is where it stops.
        // A named type, because clippy is right that the inline one is a mouthful.
        type Break = fn(&mut EntityStateWire);

        let broken: [(&str, Break); 4] = [
            ("pos.x", |state| state.pos[0] = f32::NAN),
            ("pos.y", |state| state.pos[1] = f32::INFINITY),
            ("vel.z", |state| state.vel[2] = f32::NEG_INFINITY),
            ("yaw", |state| state.yaw = f32::NAN),
        ];

        for (field, break_it) in broken {
            let mut state = EntityStateWire::at(9, 1.0);
            break_it(&mut state);

            let err = decode(&encode_entity_snapshot(1, &[state]))
                .expect_err("a non-finite component is not a position");
            assert!(
                matches!(
                    err,
                    DecodeError::NonFiniteEntity {
                        entity_id: 9,
                        field: named,
                        ..
                    } if named == field
                ),
                "breaking {field} gave {err:?}"
            );
        }
    }

    #[test]
    fn one_bad_entity_refuses_the_whole_snapshot() {
        // Unlike a malformed chunk, which is dropped with a warning. A chunk is one hole in
        // the terrain; a server that has lost track of where an entity is has lost track of
        // the world it is authoritative over, and the honest answer is to say so.
        let frame = encode_entity_snapshot(
            1,
            &[
                EntityStateWire::at(1, 0.0),
                EntityStateWire {
                    yaw: f32::NAN,
                    ..EntityStateWire::at(2, 0.0)
                },
                EntityStateWire::at(3, 0.0),
            ],
        );

        assert!(matches!(
            decode(&frame),
            Err(DecodeError::NonFiniteEntity { entity_id: 2, .. })
        ));
    }

    #[test]
    fn every_truncation_and_corruption_of_a_snapshot_survives_decoding() {
        // The most frequently sent payload in the game, and one a hostile peer gets to
        // choose every byte of. `decode` has to stay total over it.
        let frame = encode_entity_snapshot(
            9,
            &[EntityStateWire::at(1, 0.5), EntityStateWire::at(2, -3.5)],
        );

        for len in 0..frame.len() {
            assert!(
                decode(&frame[..len]).is_err(),
                "a {len}-byte prefix of a {}-byte snapshot decoded successfully",
                frame.len()
            );
        }
        assert!(decode(&frame).is_ok(), "the whole frame must still decode");

        for index in 0..frame.len() {
            for mask in [0x01u8, 0x80, 0xFF] {
                let mut corrupted = frame.clone();
                corrupted[index] ^= mask;
                // The result is uninteresting; surviving the call is the point.
                let _ = decode(&corrupted);
            }
        }
    }

    #[test]
    fn a_player_input_round_trips_and_is_recognised_as_client_only() {
        // Direction is a protocol rule: the client's own message, arriving *from* a server,
        // is a protocol error rather than something to handle.
        let input = PlayerInput {
            client_tick: 987_654,
            move_x: -0.25,
            move_z: 0.75,
            yaw: 1.5,
            pitch: -0.5,
            jump: true,
        };
        let frame = encode_player_input(&input);

        assert_eq!(decode(&frame), Ok(Message::ClientOnly("PlayerInput")));

        let envelope = fb::root_as_envelope(&frame).expect("our own encoder produces valid bytes");
        let wire = envelope
            .payload_as_player_input()
            .expect("the payload is a PlayerInput");
        assert_eq!(wire.client_tick(), input.client_tick);
        assert_eq!(wire.move_x(), input.move_x);
        assert_eq!(wire.move_z(), input.move_z);
        assert_eq!(wire.yaw(), input.yaw);
        assert_eq!(wire.pitch(), input.pitch);
        assert_eq!(wire.jump(), input.jump);
    }

    #[test]
    fn a_non_finite_intent_is_zeroed_before_it_reaches_the_wire() {
        // The invariant is a property of the contract rather than of one direction of it.
        // The server discarding a non-finite axis is no licence to send one — and zero is
        // the honest value for "this client has lost track of what the intent was".
        let frame = encode_player_input(&PlayerInput {
            client_tick: 1,
            move_x: f32::NAN,
            move_z: f32::INFINITY,
            yaw: f32::NEG_INFINITY,
            pitch: f32::NAN,
            jump: false,
        });

        let envelope = fb::root_as_envelope(&frame).expect("valid bytes");
        let wire = envelope.payload_as_player_input().expect("a PlayerInput");
        for (name, value) in [
            ("move_x", wire.move_x()),
            ("move_z", wire.move_z()),
            ("yaw", wire.yaw()),
            ("pitch", wire.pitch()),
        ] {
            assert_eq!(value, 0.0, "{name} reached the wire as {value}");
        }
    }

    #[test]
    fn a_player_input_carries_no_position_field() {
        // Structural, and the whole reason this side has no rejection path to get wrong:
        // there is nowhere in the message for a client to state where it is. Asserted
        // against the size of the encoded table, which is the only way a test can see the
        // absence of something.
        let frame = encode_player_input(&PlayerInput::default());
        assert!(
            frame.len() < 128,
            "a PlayerInput is {} bytes; a position would not fit in the contract without              showing up here",
            frame.len()
        );
    }

    #[test]
    fn chunk_data_decodes_into_a_coordinate_and_its_runs() {
        // The pairs are (block id, run length) in the index order world.fbs
        // documents; this decoder's job is to carry them out of the frame intact,
        // not to expand them.
        let runs = [3u16, 1, 0, 7, 1, 65535];
        let frame = encode_chunk_data([-2, 3, 17], &runs);

        assert_eq!(
            decode(&frame),
            Ok(Message::World(WorldUpdate::Chunk {
                coord: ChunkCoord {
                    cx: -2,
                    cy: 3,
                    cz: 17
                },
                runs: runs.to_vec(),
            }))
        );
    }

    #[test]
    fn chunk_runs_survive_the_wires_byte_order() {
        // FlatBuffers vectors are little-endian on the wire. A decoder that read
        // the raw bytes would swap every value on a big-endian host and, worse,
        // would look correct in every test run on this one.
        let runs: Vec<u16> = (0..64u16)
            .map(|i| i.wrapping_mul(0x0101) ^ 0x00FF)
            .collect();
        let frame = encode_chunk_data([0, 0, 0], &runs);

        let Ok(Message::World(WorldUpdate::Chunk { runs: decoded, .. })) = decode(&frame) else {
            panic!("that is a chunk");
        };
        assert_eq!(decoded, runs);
    }

    #[test]
    fn an_odd_run_vector_reaches_the_world_module_untouched() {
        // The RLE invariants are enforced where `chunk_size` is known — see
        // world::VoxelChunk::from_runs. This decoder must not silently repair the
        // payload on the way there, because a repaired chunk is one nobody validated.
        let frame = encode_chunk_data([0, 0, 0], &[1u16, 2, 3]);

        let Ok(Message::World(WorldUpdate::Chunk { runs, .. })) = decode(&frame) else {
            panic!("that is a chunk");
        };
        assert_eq!(runs, vec![1, 2, 3]);
    }

    #[test]
    fn an_empty_run_vector_is_carried_rather_than_confused_with_absence() {
        let frame = encode_chunk_data([0, 0, 0], &[]);

        assert_eq!(
            decode(&frame),
            Ok(Message::World(WorldUpdate::Chunk {
                coord: ChunkCoord {
                    cx: 0,
                    cy: 0,
                    cz: 0
                },
                runs: Vec::new(),
            }))
        );
    }

    #[test]
    fn chunk_data_without_runs_is_a_protocol_error() {
        // Absent is not the same as empty: an all-air chunk is one run of air.
        assert_eq!(
            decode(&encode_bare_chunk_data()),
            Err(DecodeError::MissingCoord("ChunkData")),
            "the coord is checked first, so that is what a bare table reports"
        );
    }

    #[test]
    fn chunk_data_with_a_coord_but_no_runs_is_a_protocol_error() {
        let mut builder = FlatBufferBuilder::with_capacity(64);
        let mut table = fb::ChunkDataBuilder::new(&mut builder);
        table.add_coord(&fb::ChunkCoord::new(1, 2, 3));
        let payload = table.finish();
        let frame = finish_envelope(builder, fb::Payload::ChunkData, payload.as_union_value());

        assert_eq!(decode(&frame), Err(DecodeError::MissingRuns));
    }

    #[test]
    fn a_chunk_unload_decodes_into_its_coordinate() {
        assert_eq!(
            decode(&encode_chunk_unload([5, -1, 0])),
            Ok(Message::World(WorldUpdate::Unload {
                coord: ChunkCoord {
                    cx: 5,
                    cy: -1,
                    cz: 0
                },
            }))
        );
    }

    #[test]
    fn a_chunk_unload_without_a_coord_is_a_protocol_error() {
        // Defaulting to the origin would despawn a chunk the player is standing on.
        assert_eq!(
            decode(&encode_bare_chunk_unload()),
            Err(DecodeError::MissingCoord("ChunkUnload"))
        );
    }

    // -----------------------------------------------------------------
    // Editing the world
    // -----------------------------------------------------------------

    #[test]
    fn a_block_update_decodes_into_a_world_position_and_its_block() {
        // World coordinates, signed on every axis, and negative on the vertical one
        // too: the world extends downwards and the origin is not the floor.
        assert_eq!(
            decode(&encode_block_update([-17, -3, 4096], 2)),
            Ok(Message::World(WorldUpdate::Block {
                pos: BlockCoord {
                    x: -17,
                    y: -3,
                    z: 4096
                },
                block_id: 2,
            }))
        );
    }

    #[test]
    fn a_break_arrives_as_a_placement_of_air() {
        // There is no separate break message, and that is the contract rather than a
        // shortcut: one shape covers both directions of change, so the union has one
        // fewer member for both sides to keep exhaustive.
        assert_eq!(
            decode(&encode_block_update([1, 2, 3], 0)),
            Ok(Message::World(WorldUpdate::Block {
                pos: BlockCoord { x: 1, y: 2, z: 3 },
                block_id: 0,
            }))
        );
    }

    #[test]
    fn a_block_update_without_a_position_is_a_protocol_error() {
        // Defaulting to the origin would rewrite a voxel nobody named, somewhere a
        // player might be standing. `schemas/world.fbs` requires the refusal.
        assert_eq!(
            decode(&encode_bare_block_update()),
            Err(DecodeError::MissingBlockPos("BlockUpdate"))
        );
    }

    #[test]
    fn a_block_id_this_build_does_not_know_survives_the_decoder() {
        // A newer server, not a corrupt frame. The palette lookup is bounds-checked
        // and loud, so carrying the id through is strictly better than dropping the
        // update and leaving the voxel showing what it used to be.
        let Ok(Message::World(WorldUpdate::Block { block_id, .. })) =
            decode(&encode_block_update([0, 0, 0], u16::MAX))
        else {
            panic!("that is a block update");
        };
        assert_eq!(block_id, u16::MAX);
    }

    #[test]
    fn every_truncation_and_corruption_of_a_block_update_survives_decoding() {
        let frame = encode_block_update([-5, 70, 12], 3);

        for len in 0..frame.len() {
            assert!(
                decode(&frame[..len]).is_err(),
                "a {len}-byte prefix of a {}-byte block update decoded successfully",
                frame.len()
            );
        }
        assert!(decode(&frame).is_ok(), "the whole frame must still decode");

        for index in 0..frame.len() {
            for mask in [0x01u8, 0x80, 0xFF] {
                let mut corrupted = frame.clone();
                corrupted[index] ^= mask;
                // The result is uninteresting; surviving the call is the point.
                let _ = decode(&corrupted);
            }
        }
    }

    fn edit(action: EditAction) -> BlockEditRequest {
        BlockEditRequest {
            pos: BlockCoord { x: -4, y: 71, z: 9 },
            action,
            slot: 7,
            client_tick: 4321,
        }
    }

    #[test]
    fn an_edit_request_round_trips_and_is_recognised_as_client_only() {
        // Direction is a protocol rule: the client's own message, arriving *from* a
        // server, is a protocol error rather than something to handle.
        let frame = encode_block_edit_request(&edit(EditAction::Break));
        assert_eq!(decode(&frame), Ok(Message::ClientOnly("BlockEditRequest")));

        let envelope = fb::root_as_envelope(&frame).expect("our own encoder produces valid bytes");
        let wire = envelope
            .payload_as_block_edit_request()
            .expect("the payload is a BlockEditRequest");
        let pos = wire.pos().expect("the position is always written");
        assert_eq!((pos.x(), pos.y(), pos.z()), (-4, 71, 9));
        assert_eq!(wire.slot(), 7);
        assert_eq!(wire.client_tick(), 4321);
    }

    #[test]
    fn each_action_reaches_the_wire_as_the_schemas_own_value() {
        // The tags are the contract's, not this module's: a break encoded as `Place`
        // would compile perfectly and dig nothing.
        for (action, expected) in [
            (EditAction::Break, fb::EditAction::Break),
            (EditAction::Place, fb::EditAction::Place),
        ] {
            let frame = encode_block_edit_request(&edit(action));
            let envelope = fb::root_as_envelope(&frame).expect("valid bytes");
            let wire = envelope
                .payload_as_block_edit_request()
                .expect("a BlockEditRequest");

            assert_eq!(wire.action(), expected, "{action:?}");
            assert_ne!(
                wire.action(),
                fb::EditAction::Unknown,
                "{action:?} reached the wire as the value the server fails closed on"
            );
        }
    }

    #[test]
    fn an_edit_request_carries_no_outcome_and_no_position_of_the_player() {
        // Structural, the same way `a_player_input_carries_no_position_field` is: the
        // request is a point and an action, so there is nowhere in it for a client to
        // state that the edit was legal or where it was standing when it asked.
        let frame = encode_block_edit_request(&edit(EditAction::Place));
        assert!(
            frame.len() < 128,
            "a BlockEditRequest is {} bytes; anything the server would have to \
             disbelieve would show up here",
            frame.len()
        );
    }

    #[test]
    fn mining_intent_round_trips_as_a_client_only_payload() {
        let request = MineRequest {
            pos: BlockCoord { x: -8, y: 3, z: 99 },
            active: true,
            client_tick: 123,
            // Not slot zero, so a field that was dropped on the way out reads as absent
            // rather than as the value that happened to be the default.
            slot: 5,
        };
        let frame = encode_mine_request(&request);
        assert_eq!(decode(&frame), Ok(Message::ClientOnly("MineRequest")));

        let envelope = fb::root_as_envelope(&frame).expect("valid bytes");
        let wire = envelope.payload_as_mine_request().expect("a MineRequest");
        let pos = wire.pos().expect("the encoder always writes the position");
        assert_eq!((pos.x(), pos.y(), pos.z()), (-8, 3, 99));
        assert!(wire.active());
        assert_eq!(wire.client_tick(), 123);
        assert_eq!(
            wire.slot(),
            5,
            "the slot the player is mining with never left the client"
        );
    }

    #[test]
    fn mining_progress_decodes_and_requires_a_position() {
        assert_eq!(
            decode(&encode_mine_progress(Some([-1, 4, 8]), 255)),
            Ok(Message::MineProgress(MineProgress {
                pos: BlockCoord { x: -1, y: 4, z: 8 },
                progress: 255,
            }))
        );
        assert_eq!(
            decode(&encode_mine_progress(None, 17)),
            Err(DecodeError::MissingMinePos)
        );
    }

    #[test]
    fn inventory_move_intent_round_trips_as_a_client_only_payload() {
        let request = InventoryMoveRequest {
            from: 3,
            to: 35,
            count: 64,
        };
        let frame = encode_inventory_move_request(&request);
        assert_eq!(
            decode(&frame),
            Ok(Message::ClientOnly("InventoryMoveRequest"))
        );

        let envelope = fb::root_as_envelope(&frame).expect("valid bytes");
        let wire = envelope
            .payload_as_inventory_move_request()
            .expect("an InventoryMoveRequest");
        assert_eq!((wire.from(), wire.to(), wire.count()), (3, 35, 64));
    }

    #[test]
    fn a_resend_request_round_trips_and_is_recognised_as_client_only() {
        // Direction is a protocol rule: this client's own message, arriving *from* a
        // server, is a protocol error rather than something to handle. The union is
        // shared, so the classification is the only thing that says which way it travels.
        let frame = encode_chunk_resend_request(ChunkCoord {
            cx: -4,
            cy: 12,
            cz: 900,
        });
        assert_eq!(
            decode(&frame),
            Ok(Message::ClientOnly("ChunkResendRequest"))
        );

        let envelope = fb::root_as_envelope(&frame).expect("our own encoder produces valid bytes");
        let wire = envelope
            .payload_as_chunk_resend_request()
            .expect("the payload is a ChunkResendRequest");
        let coord = wire.coord().expect("the coordinate is always written");
        assert_eq!((coord.cx(), coord.cy(), coord.cz()), (-4, 12, 900));
    }

    #[test]
    fn a_resend_request_carries_nothing_but_a_coordinate() {
        // Structural, the same way `an_edit_request_carries_no_outcome_and_no_position_of_the_player`
        // is: there is nowhere in this message for a client to state what the chunk holds,
        // how urgently it wants it, or that it is entitled to it.
        let frame = encode_chunk_resend_request(ChunkCoord {
            cx: 1,
            cy: 2,
            cz: 3,
        });
        assert!(
            frame.len() < 64,
            "a ChunkResendRequest is {} bytes; anything the server would have to \
             disbelieve would show up here",
            frame.len()
        );
    }

    #[test]
    fn an_inventory_state_decodes_into_complete_stacks() {
        let frame = encode_inventory_state(Some(&[1, 4, 0, 0, 1, 65_535]));
        assert_eq!(
            decode(&frame),
            Ok(Message::Inventory(InventoryState {
                stacks: vec![
                    InventoryStack {
                        item_id: 1,
                        count: 4,
                        ..Default::default()
                    },
                    InventoryStack {
                        item_id: 0,
                        count: 0,
                        ..Default::default()
                    },
                    InventoryStack {
                        item_id: 1,
                        count: 65_535,
                        ..Default::default()
                    },
                ],
            }))
        );
    }

    #[test]
    fn absent_and_empty_inventory_vectors_are_both_empty_inventories() {
        for frame in [
            encode_inventory_state(None),
            encode_inventory_state(Some(&[])),
        ] {
            assert_eq!(
                decode(&frame),
                Ok(Message::Inventory(InventoryState { stacks: vec![] }))
            );
        }
    }

    #[test]
    fn malformed_inventory_shapes_are_refused_at_the_decode_boundary() {
        for (values, want) in [
            (vec![1], DecodeError::InventoryLength(1)),
            (
                vec![0, 1],
                DecodeError::InventorySlotPair {
                    slot: 0,
                    item_id: 0,
                    count: 1,
                },
            ),
            (
                vec![2, 0],
                DecodeError::InventorySlotPair {
                    slot: 0,
                    item_id: 2,
                    count: 0,
                },
            ),
        ] {
            assert_eq!(decode(&encode_inventory_state(Some(&values))), Err(want));
        }
    }

    #[test]
    fn every_truncation_of_a_chunk_frame_is_an_error_and_never_a_panic() {
        // The chunk payload is the largest thing a peer gets to choose the shape
        // of, so `decode` has to stay total over it too.
        let frame = encode_chunk_data([0, 2, 0], &[1u16, 32768]);
        for len in 0..frame.len() {
            assert!(
                decode(&frame[..len]).is_err(),
                "a {len}-byte prefix of a {}-byte chunk frame decoded successfully",
                frame.len()
            );
        }
        assert!(decode(&frame).is_ok(), "the whole frame must still decode");
    }

    #[test]
    fn every_single_byte_corruption_of_a_chunk_frame_survives_decoding() {
        let frame = encode_chunk_data([0, 2, 0], &[1u16, 4, 0, 32764]);
        for index in 0..frame.len() {
            for mask in [0x01u8, 0x80, 0xFF] {
                let mut corrupted = frame.clone();
                corrupted[index] ^= mask;
                let _ = decode(&corrupted);
            }
        }
    }

    #[test]
    fn an_empty_frame_is_too_short() {
        assert_eq!(decode(&[]), Err(DecodeError::TooShort { len: 0 }));
    }

    #[test]
    fn anything_shorter_than_a_root_and_a_tag_is_too_short() {
        for len in 0..8 {
            let frame = vec![0u8; len];
            assert_eq!(
                decode(&frame),
                Err(DecodeError::TooShort { len }),
                "{len} bytes"
            );
        }
    }

    #[test]
    fn garbage_of_a_plausible_length_is_not_a_voxelheim_buffer() {
        let frame = vec![0x41u8; 64];
        assert_eq!(decode(&frame), Err(DecodeError::NotVoxelheim));
    }

    #[test]
    fn a_correct_tag_over_nonsense_offsets_is_malformed() {
        // The tag sits at bytes 4..8, so this is the shape of an attack: pass the
        // cheap check, then hand the verifier a root offset pointing nowhere.
        let mut frame = vec![0xFFu8; 32];
        frame[..4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        frame[4..8].copy_from_slice(fb::ENVELOPE_IDENTIFIER.as_bytes());

        assert!(
            matches!(decode(&frame), Err(DecodeError::Malformed(_))),
            "the verifier must refuse it"
        );
    }

    #[test]
    fn every_truncation_of_a_valid_frame_is_an_error_and_never_a_panic() {
        // The untrusted-input boundary, exercised the way a network actually
        // breaks things. `decode` must be total: no panic, no index out of range,
        // for any prefix of anything.
        let frame = encode_server_welcome(&WelcomeWire::default());
        for len in 0..frame.len() {
            assert!(
                decode(&frame[..len]).is_err(),
                "a {len}-byte prefix of a {}-byte frame decoded successfully",
                frame.len()
            );
        }
        assert!(decode(&frame).is_ok(), "the whole frame must still decode");
    }

    #[test]
    fn every_single_byte_corruption_is_an_error_or_a_message_but_never_a_panic() {
        let frame = encode_server_welcome(&WelcomeWire::default());
        for index in 0..frame.len() {
            for mask in [0x01u8, 0x80, 0xFF] {
                let mut corrupted = frame.clone();
                corrupted[index] ^= mask;
                // The result is uninteresting; surviving the call is the point.
                let _ = decode(&corrupted);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Protocol V3 — attack intent, vitals, mobs and durability
    // -----------------------------------------------------------------------

    /// Direction is a protocol rule: a swing is something *this* client sends, so one
    /// arriving from a server is a protocol error rather than something to handle.
    /// There is no attack acknowledgement and no damage-result payload to mistake it for.
    #[test]
    fn an_attack_request_from_a_server_is_a_protocol_error() {
        assert_eq!(
            decode(&encode_attack_request(&AttackRequest {
                slot: 0,
                client_tick: 12,
            })),
            Ok(Message::ClientOnly("AttackRequest"))
        );
    }

    // -----------------------------------------------------------------------
    // Protocol V4 — craft intent
    // -----------------------------------------------------------------------

    /// Every member this client can name, and the frame each one produces.
    ///
    /// Read back through the generated accessors rather than through [`decode`], because
    /// `decode` is the *inbound* path and a craft never arrives here. What matters is the
    /// bytes the server will read: the recipe member and the tick, and nothing else.
    ///
    /// **The table is swept against `RecipeID::ENUM_VALUES` rather than counted**, and the
    /// loop at the bottom is that sweep. A hand-written mirror of an appended-to contract
    /// fails in exactly one direction — a member arrives and the mirror does not gain it —
    /// and a count of this table cannot see that happen, because the count and the table
    /// are the same hand. #113 is what that cost: the campfire was on the wire from V6 and
    /// this client could not name it.
    #[test]
    fn a_craft_request_carries_one_recipe_member_and_a_tick() {
        let named = [
            (RecipeId::Forge, fb::RecipeID::Forge),
            (RecipeId::IronSword, fb::RecipeID::IronSword),
            (RecipeId::SharpeningStone, fb::RecipeID::SharpeningStone),
            (RecipeId::Tent, fb::RecipeID::Tent),
            (RecipeId::Campfire, fb::RecipeID::Campfire),
            (RecipeId::LeatherPatch, fb::RecipeID::LeatherPatch),
            (RecipeId::Shovel, fb::RecipeID::Shovel),
            (RecipeId::Pickaxe, fb::RecipeID::Pickaxe),
            (RecipeId::Axe, fb::RecipeID::Axe),
        ];

        for (recipe, wire) in named {
            let frame = encode_craft_request(&CraftRequest {
                recipe,
                client_tick: 41,
            });
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            assert_eq!(envelope.payload_type(), fb::Payload::CraftRequest);
            let request = envelope
                .payload_as_craft_request()
                .expect("the payload is a craft request");
            assert_eq!(request.recipe(), wire, "{recipe:?}");
            assert_eq!(request.client_tick(), 41);

            // The absent-field zero the server fails closed on is never what leaves here.
            // `RecipeId` cannot express it, and this is the frame that proves the mapping
            // preserves that rather than collapsing two members onto one.
            assert_ne!(request.recipe(), fb::RecipeID::Unknown, "{recipe:?}");
        }

        // Every member the contract names is one this client can ask for. `Unknown` is
        // skipped because it is the absent-field case rather than a recipe, and it is
        // deliberately unrepresentable on this side.
        for member in fb::RecipeID::ENUM_VALUES {
            if *member == fb::RecipeID::Unknown {
                continue;
            }
            assert!(
                named.iter().any(|(_, wire)| wire == member),
                "the contract names {} and this client cannot ask for it",
                member.variant_name().unwrap_or("a member past the end")
            );
        }
    }

    // -----------------------------------------------------------------------
    // Protocol V4 — repair intent
    // -----------------------------------------------------------------------

    /// Two slot indexes and a tick, and nothing that could be a durability.
    ///
    /// Read back through the generated accessors rather than through [`decode`], for the
    /// reason a craft is: this is an outbound message and one never arrives here. The
    /// out-of-range pair is in the table deliberately — `schemas/player.fbs` asks for both
    /// indexes verbatim, so a slot past the end of the pack is an ordinary refusal in the
    /// simulation rather than something this encoder gets to clamp or drop.
    #[test]
    fn a_repair_request_carries_two_slots_and_a_tick_verbatim() {
        for (kit_slot, target_slot, client_tick) in [(0, 1, 41), (3, 2, 0), (255, 254, u32::MAX)] {
            let frame = encode_repair_request(&RepairRequest {
                kit_slot,
                target_slot,
                client_tick,
            });
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            assert_eq!(envelope.payload_type(), fb::Payload::RepairRequest);
            let request = envelope
                .payload_as_repair_request()
                .expect("the payload is a repair request");
            assert_eq!(request.kit_slot(), kit_slot);
            assert_eq!(request.target_slot(), target_slot);
            assert_eq!(request.client_tick(), client_tick);
        }
    }

    #[test]
    fn a_snapshot_decodes_its_mobs_in_wire_order() {
        let mobs = [
            MobStateWire {
                vel: Some([1.0, -2.0, 0.5]),
                yaw: 1.25,
                health: 35,
                action: fb::MobAction::Chase,
                ..MobStateWire::draugr(900, 8.5)
            },
            MobStateWire {
                action: fb::MobAction::Windup,
                health: 1,
                ..MobStateWire::draugr(901, -30.0)
            },
        ];

        let Ok(Message::Snapshot(snapshot)) = snapshot_of(&mobs, PlayerVitalsWire::default())
        else {
            panic!("a valid snapshot did not decode");
        };

        assert_eq!(
            snapshot.mobs,
            vec![
                MobState {
                    entity_id: 900,
                    kind: MobKind::Draugr,
                    pos: [8.5, 64.0, 0.0],
                    vel: [1.0, -2.0, 0.5],
                    yaw: 1.25,
                    health: 35,
                    max_health: 60,
                    action: MobAction::Chase,
                },
                MobState {
                    entity_id: 901,
                    kind: MobKind::Draugr,
                    pos: [-30.0, 64.0, 0.0],
                    vel: [0.0, 0.0, 0.0],
                    yaw: 0.0,
                    health: 1,
                    max_health: 60,
                    action: MobAction::Windup,
                },
            ]
        );
    }

    /// A snapshot carrying a vargr decodes, where until legacy PR 172 it ended the session.
    ///
    /// **This is the whole of the urgency behind that issue, in one frame.** The server's
    /// spawn director does not hold the vargr back for the night, so the first one to
    /// wander into view produced `UnknownMobEnum`, and `session.rs` turns any decode error
    /// into a `protocol_failure` — it does not skip the mob and it does not drop the
    /// snapshot, it drops the connection. Asserted at the *snapshot* level rather than on
    /// `from_wire` alone, because that is the layer the session actually calls.
    #[test]
    fn a_snapshot_carrying_a_vargr_decodes_rather_than_ending_the_session() {
        let vargr = MobStateWire {
            kind: fb::MobKind::Vargr,
            health: 35,
            max_health: 35,
            action: fb::MobAction::Chase,
            ..MobStateWire::draugr(902, 4.0)
        };

        let Ok(Message::Snapshot(snapshot)) = snapshot_of(
            &[MobStateWire::draugr(900, 8.5), vargr],
            PlayerVitalsWire::default(),
        ) else {
            panic!("a snapshot carrying a vargr did not decode");
        };

        assert_eq!(
            snapshot
                .mobs
                .iter()
                .map(|mob| (mob.entity_id, mob.kind))
                .collect::<Vec<_>>(),
            vec![(900, MobKind::Draugr), (902, MobKind::Vargr)]
        );
    }

    /// The campfire's half of the same statement, at the same layer.
    #[test]
    fn a_snapshot_carrying_a_campfire_decodes() {
        let mut campfire = StructureStateWire::tent(903, 7);
        campfire.kind = fb::StructureKind::Campfire;
        campfire.facing = fb::Facing::South;

        let Ok(Message::Snapshot(snapshot)) = structures_of(&[campfire]) else {
            panic!("a snapshot carrying a campfire did not decode");
        };

        assert_eq!(
            snapshot.structures,
            vec![StructureState {
                structure_id: 903,
                kind: StructureKind::Campfire,
                anchor: BlockCoord { x: 4, y: 63, z: -7 },
                facing: Facing::South,
                owner_entity_id: 7,
            }]
        );
    }

    /// A snapshot with no mob vector at all is a session that can see no mobs, exactly
    /// as an absent entity vector is a session that can see nobody. Absence is not an
    /// error here, and the empty set is what the client despawns against. The structure
    /// vector obeys the same rule, and for the same reason: a camp nobody has pitched
    /// yet is an ordinary state.
    #[test]
    fn a_snapshot_with_no_mobs_or_structures_carries_none() {
        for frame in [
            encode_bare_entity_snapshot(5),
            encode_entity_snapshot_with(5, &[], &[], &[], PlayerVitalsWire::default(), &[]),
        ] {
            let Ok(Message::Snapshot(snapshot)) = decode(&frame) else {
                panic!("a valid snapshot did not decode");
            };
            assert!(snapshot.mobs.is_empty());
            assert!(snapshot.structures.is_empty());
        }
    }

    /// The `(required)` field, refused by the verifier before any field of the snapshot
    /// is read. No generated builder can produce this frame — a peer can.
    #[test]
    fn a_snapshot_without_vitals_is_a_protocol_error() {
        let err = decode(&encode_entity_snapshot_without_vitals(1))
            .expect_err("a snapshot with no self_vitals is not a usable message");

        let DecodeError::Malformed(detail) = &err else {
            panic!("got {err:?}, want a Malformed verifier refusal");
        };
        assert!(
            detail.contains("self_vitals"),
            "the refusal should name the missing field, got {detail}"
        );
    }

    #[test]
    fn a_dead_players_countdown_decodes_whole() {
        let Ok(Message::Snapshot(snapshot)) = snapshot_of(
            &[],
            PlayerVitalsWire {
                health: 0,
                max_health: 100,
                life_state: fb::LifeState::Dead,
                respawn_ticks: 60,
                invulnerable: false,
            },
        ) else {
            panic!("a valid dead player's snapshot did not decode");
        };

        assert_eq!(
            snapshot.self_vitals,
            PlayerVitals {
                health: 0,
                max_health: 100,
                life_state: LifeState::Dead,
                respawn_ticks: 60,
                invulnerable: false,
            }
        );
    }

    /// Server-owned respawn protection is a flag the client reads, never a timer it runs.
    #[test]
    fn respawn_protection_arrives_as_the_servers_answer() {
        let Ok(Message::Snapshot(snapshot)) = snapshot_of(
            &[],
            PlayerVitalsWire {
                health: 100,
                invulnerable: true,
                ..PlayerVitalsWire::default()
            },
        ) else {
            panic!("a valid snapshot did not decode");
        };
        assert!(snapshot.self_vitals.invulnerable);
        assert_eq!(snapshot.self_vitals.respawn_ticks, 0);
    }

    #[test]
    fn malformed_vitals_are_protocol_errors() {
        for (name, vitals, want) in [
            (
                "no life state at all, which is what an absent field decodes as",
                PlayerVitalsWire {
                    life_state: fb::LifeState::Unknown,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::UnknownLifeState,
            ),
            (
                "a member a newer contract added",
                PlayerVitalsWire {
                    life_state: fb::LifeState(9),
                    ..PlayerVitalsWire::default()
                },
                DecodeError::UnknownLifeState,
            ),
            (
                "a zero maximum, which is the division a health bar performs",
                PlayerVitalsWire {
                    health: 0,
                    max_health: 0,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::VitalsHealth {
                    health: 0,
                    max_health: 0,
                },
            ),
            (
                "more health than the maximum",
                PlayerVitalsWire {
                    health: 101,
                    max_health: 100,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::VitalsHealth {
                    health: 101,
                    max_health: 100,
                },
            ),
            (
                "alive with nothing left, which is a server that lost track of its own player",
                PlayerVitalsWire {
                    health: 0,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::AliveWithoutHealth,
            ),
            (
                "a respawn countdown for someone who is not dead",
                PlayerVitalsWire {
                    respawn_ticks: 40,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::RespawnWhileAlive { respawn_ticks: 40 },
            ),
        ] {
            assert_eq!(snapshot_of(&[], vitals), Err(want), "{name}");
        }
    }

    #[test]
    fn malformed_mobs_are_protocol_errors() {
        let vitals = PlayerVitalsWire::default();
        for (name, mob, want) in [
            (
                "the reserved identity",
                MobStateWire {
                    entity_id: 0,
                    ..MobStateWire::draugr(1, 0.0)
                },
                DecodeError::MobWithoutIdentity,
            ),
            (
                "no position, which is never read as the origin",
                MobStateWire {
                    pos: None,
                    ..MobStateWire::draugr(7, 0.0)
                },
                DecodeError::MissingMobTransform {
                    entity_id: 7,
                    field: "pos",
                },
            ),
            (
                "no velocity",
                MobStateWire {
                    vel: None,
                    ..MobStateWire::draugr(7, 0.0)
                },
                DecodeError::MissingMobTransform {
                    entity_id: 7,
                    field: "vel",
                },
            ),
            (
                "a kind this build does not speak",
                MobStateWire {
                    kind: fb::MobKind::Unknown,
                    ..MobStateWire::draugr(7, 0.0)
                },
                DecodeError::UnknownMobEnum {
                    entity_id: 7,
                    field: "kind",
                    value: 0,
                },
            ),
            (
                "an action a newer contract added",
                MobStateWire {
                    action: fb::MobAction(200),
                    ..MobStateWire::draugr(7, 0.0)
                },
                DecodeError::UnknownMobEnum {
                    entity_id: 7,
                    field: "action",
                    value: 200,
                },
            ),
            (
                "a zero maximum",
                MobStateWire {
                    health: 0,
                    max_health: 0,
                    ..MobStateWire::draugr(7, 0.0)
                },
                DecodeError::MobHealth {
                    entity_id: 7,
                    health: 0,
                    max_health: 0,
                },
            ),
            (
                "more health than the maximum",
                MobStateWire {
                    health: 61,
                    ..MobStateWire::draugr(7, 0.0)
                },
                DecodeError::MobHealth {
                    entity_id: 7,
                    health: 61,
                    max_health: 60,
                },
            ),
        ] {
            assert_eq!(snapshot_of(&[mob], vitals), Err(want), "{name}");
        }
    }

    /// A finiteness test and never a clamp, in this direction too: `NaN` compares false
    /// against every bound, so a clamp would pass one through into the interpolation and
    /// from there into a `Transform` it never leaves.
    #[test]
    fn a_non_finite_mob_is_a_protocol_error() {
        type Break = fn(&mut MobStateWire);
        for (field, apply) in [
            ("pos.x", (|m| m.pos = Some([f32::NAN, 64.0, 0.0])) as Break),
            ("pos.y", |m| m.pos = Some([0.0, f32::INFINITY, 0.0])),
            ("vel.z", |m| m.vel = Some([0.0, 0.0, f32::NEG_INFINITY])),
            ("yaw", |m| m.yaw = f32::NAN),
        ] {
            let mut mob = MobStateWire::draugr(9, 0.0);
            apply(&mut mob);

            let err = snapshot_of(&[mob], PlayerVitalsWire::default())
                .expect_err("a non-finite mob is not a usable message");
            let DecodeError::NonFiniteMob {
                entity_id,
                field: got,
                ..
            } = err
            else {
                panic!("a non-finite {field} was refused as {err:?}");
            };
            assert_eq!(entity_id, 9);
            assert_eq!(got, field);
        }
    }

    /// "Globally unique" is a claim about the whole snapshot, and an id that names two
    /// things is a client that would spawn one body for both of them.
    #[test]
    fn a_mob_may_not_share_an_id_with_anything_else_in_the_snapshot() {
        let vitals = PlayerVitalsWire::default();

        // With a player.
        assert_eq!(
            decode(&encode_entity_snapshot_with(
                1,
                &[EntityStateWire::at(42, 0.0)],
                &[],
                &[MobStateWire::draugr(42, 0.0)],
                vitals,
                &[],
            )),
            Err(DecodeError::MobEntityConflict(42))
        );

        // With a drop.
        assert_eq!(
            decode(&encode_entity_snapshot_with(
                1,
                &[],
                &[ItemDropStateWire::item(42, 3)],
                &[MobStateWire::draugr(42, 0.0)],
                vitals,
                &[],
            )),
            Err(DecodeError::MobEntityConflict(42))
        );

        // With another mob.
        assert_eq!(
            snapshot_of(
                &[MobStateWire::draugr(42, 0.0), MobStateWire::draugr(42, 5.0)],
                vitals
            ),
            Err(DecodeError::MobEntityConflict(42))
        );
    }

    // ---------------------------------------------------------------------------
    // Structures
    // ---------------------------------------------------------------------------

    #[test]
    fn a_structure_decodes_with_its_anchor_kind_facing_and_owner() {
        let mut forge = StructureStateWire::tent(900, 7);
        forge.kind = fb::StructureKind::Forge;
        forge.facing = fb::Facing::West;
        forge.anchor = Some([-3, 71, 12]);

        let Ok(Message::Snapshot(snapshot)) =
            structures_of(&[StructureStateWire::tent(901, 7), forge])
        else {
            panic!("a valid structure vector did not decode");
        };

        assert_eq!(
            snapshot.structures,
            vec![
                StructureState {
                    structure_id: 901,
                    kind: StructureKind::Tent,
                    anchor: BlockCoord { x: 4, y: 63, z: -7 },
                    facing: Facing::North,
                    owner_entity_id: 7,
                },
                StructureState {
                    structure_id: 900,
                    kind: StructureKind::Forge,
                    anchor: BlockCoord {
                        x: -3,
                        y: 71,
                        z: 12,
                    },
                    facing: Facing::West,
                    owner_entity_id: 7,
                },
            ]
        );
    }

    #[test]
    fn a_structure_whose_owner_is_offline_decodes() {
        // Zero is legal in this one field from V5 on: `schemas/player.fbs` reads it
        // as "the owner has no live session right now", not as "unowned". Until
        // this decoded, the first snapshot naming an offline owner would have ended
        // the session as a protocol failure — which is what a server that can send
        // one is waiting on (legacy PR 148).
        let Ok(Message::Snapshot(snapshot)) = structures_of(&[StructureStateWire::tent(900, 0)])
        else {
            panic!("an offline owner is not a malformed structure");
        };

        assert_eq!(snapshot.structures.len(), 1);
        assert_eq!(snapshot.structures[0].owner_entity_id, 0);
    }

    #[test]
    fn a_snapshot_may_mix_offline_and_live_owners() {
        // Both in one vector, because that is what a world with someone logged out
        // looks like. What each one *means* to a session is `player/structures.rs`'s
        // question, and it is answered by a test there rather than by a copy of its
        // comparison here.
        let Ok(Message::Snapshot(snapshot)) = structures_of(&[
            StructureStateWire::tent(900, 0),
            StructureStateWire::tent(901, 7),
        ]) else {
            panic!("a mixed vector of owners must decode");
        };

        let owners: Vec<u64> = snapshot
            .structures
            .iter()
            .map(|state| state.owner_entity_id)
            .collect();
        assert_eq!(owners, vec![0, 7]);
    }

    /// Every invariant `schemas/player.fbs` attaches to `StructureState`, one broken
    /// field at a time. Each is refused rather than defaulted, because a default anchor
    /// is a camp at the origin nobody pitched and a default kind is a shelter the server
    /// never said was there.
    ///
    /// `owner_entity_id` is not among them any more: V5 makes 0 a legal value there,
    /// and the two tests above are what took its place.
    #[test]
    fn a_structure_with_a_broken_field_is_refused_rather_than_defaulted() {
        let broken = |mutate: fn(&mut StructureStateWire)| {
            let mut wire = StructureStateWire::tent(900, 7);
            mutate(&mut wire);
            structures_of(&[wire])
        };

        assert_eq!(
            broken(|wire| wire.structure_id = 0),
            Err(DecodeError::StructureWithoutIdentity)
        );
        assert_eq!(
            broken(|wire| wire.anchor = None),
            Err(DecodeError::MissingStructureAnchor(900))
        );
        assert_eq!(
            broken(|wire| wire.kind = fb::StructureKind::Unknown),
            Err(DecodeError::UnknownStructureEnum {
                structure_id: 900,
                field: "kind",
                value: 0,
            })
        );
        assert_eq!(
            broken(|wire| wire.kind = fb::StructureKind(9)),
            Err(DecodeError::UnknownStructureEnum {
                structure_id: 900,
                field: "kind",
                value: 9,
            })
        );
        assert_eq!(
            broken(|wire| wire.facing = fb::Facing::Unknown),
            Err(DecodeError::UnknownStructureEnum {
                structure_id: 900,
                field: "facing",
                value: 0,
            })
        );
        assert_eq!(
            broken(|wire| wire.facing = fb::Facing(9)),
            Err(DecodeError::UnknownStructureEnum {
                structure_id: 900,
                field: "facing",
                value: 9,
            })
        );
    }

    /// "Globally unique" is a claim about the whole snapshot, and it covers structures
    /// too: an id that names a tent and a draugr is a client that would draw one over
    /// the other.
    #[test]
    fn a_structure_may_not_share_an_id_with_anything_else_in_the_snapshot() {
        let vitals = PlayerVitalsWire::default();
        let tent = |id| StructureStateWire::tent(id, 7);

        // With a player.
        assert_eq!(
            decode(&encode_entity_snapshot_with(
                1,
                &[EntityStateWire::at(42, 0.0)],
                &[],
                &[],
                vitals,
                &[tent(42)],
            )),
            Err(DecodeError::StructureEntityConflict(42))
        );

        // With a drop.
        assert_eq!(
            decode(&encode_entity_snapshot_with(
                1,
                &[],
                &[ItemDropStateWire::item(42, 3)],
                &[],
                vitals,
                &[tent(42)],
            )),
            Err(DecodeError::StructureEntityConflict(42))
        );

        // With a mob.
        assert_eq!(
            decode(&encode_entity_snapshot_with(
                1,
                &[],
                &[],
                &[MobStateWire::draugr(42, 0.0)],
                vitals,
                &[tent(42)],
            )),
            Err(DecodeError::StructureEntityConflict(42))
        );

        // With another structure.
        assert_eq!(
            structures_of(&[tent(42), tent(42)]),
            Err(DecodeError::StructureEntityConflict(42))
        );
    }

    /// The whole request, read back out of the bytes that would leave: the server acts on
    /// the frame, so the frame is what a test asserts.
    #[test]
    fn a_place_structure_request_carries_the_anchor_facing_slot_and_tick() {
        for (facing, wire) in [
            (Facing::North, fb::Facing::North),
            (Facing::East, fb::Facing::East),
            (Facing::South, fb::Facing::South),
            (Facing::West, fb::Facing::West),
        ] {
            let frame = encode_place_structure_request(&PlaceStructureRequest {
                slot: 3,
                anchor: BlockCoord { x: 8, y: 62, z: -4 },
                facing,
                client_tick: 77,
            });

            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            assert_eq!(envelope.payload_type(), fb::Payload::PlaceStructureRequest);
            let request = envelope
                .payload_as_place_structure_request()
                .expect("the payload the tag names");
            assert_eq!(request.slot(), 3);
            assert_eq!(request.facing(), wire);
            assert_eq!(request.client_tick(), 77);
            let anchor = request.anchor().expect("the anchor is always written");
            assert_eq!((anchor.x(), anchor.y(), anchor.z()), (8, 62, -4));
        }
    }

    #[test]
    fn a_remove_structure_request_carries_the_id_and_the_tick() {
        let frame = encode_remove_structure_request(&RemoveStructureRequest {
            structure_id: 4_242,
            client_tick: 9,
        });

        let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
        assert_eq!(envelope.payload_type(), fb::Payload::RemoveStructureRequest);
        let request = envelope
            .payload_as_remove_structure_request()
            .expect("the payload the tag names");
        assert_eq!(request.structure_id(), 4_242);
        assert_eq!(request.client_tick(), 9);
    }

    /// Corruption anywhere in a snapshot carrying every V4 vector: an error or a
    /// message, never a panic. The mob vector is the first vector of *tables* on this
    /// wire, so its offsets are new bytes for a peer to get wrong — and the structure
    /// vector is the second, with an optional struct field inside each of its tables.
    #[test]
    fn every_truncation_and_corruption_of_a_v4_snapshot_survives_decoding() {
        let frame = encode_entity_snapshot_with(
            7,
            &[EntityStateWire::at(1, 0.0)],
            &[ItemDropStateWire::item(2, 3)],
            &[MobStateWire::draugr(4, 1.0)],
            PlayerVitalsWire::default(),
            &[StructureStateWire::tent(5, 1)],
        );

        for len in 0..frame.len() {
            let _ = decode(&frame[..len]);
        }
        for index in 0..frame.len() {
            let mut damaged = frame.clone();
            damaged[index] ^= 0xFF;
            let _ = decode(&damaged);
        }
    }

    #[test]
    fn an_inventory_decodes_durability_beside_each_stack() {
        let frame = encode_inventory_state_with_durability(
            // A sword, a resource stack, an empty slot.
            Some(&[7, 1, 2, 64, 0, 0]),
            Some(&[35, 0, 0]),
            Some(&[100, 0, 0]),
        );

        assert_eq!(
            decode(&frame),
            Ok(Message::Inventory(InventoryState {
                stacks: vec![
                    InventoryStack {
                        item_id: 7,
                        count: 1,
                        durability: 35,
                        max_durability: 100,
                    },
                    InventoryStack {
                        item_id: 2,
                        count: 64,
                        ..Default::default()
                    },
                    InventoryStack::default(),
                ],
            }))
        );
    }

    /// Zero durability under a non-zero maximum is a worn-out item: unusable, still
    /// carried, still in its slot. It is not an empty slot and must never decode as one.
    #[test]
    fn a_worn_out_item_is_legal_and_is_not_an_empty_slot() {
        let frame = encode_inventory_state_with_durability(Some(&[7, 1]), Some(&[0]), Some(&[100]));

        assert_eq!(
            decode(&frame),
            Ok(Message::Inventory(InventoryState {
                stacks: vec![InventoryStack {
                    item_id: 7,
                    count: 1,
                    durability: 0,
                    max_durability: 100,
                }],
            }))
        );
    }

    /// Refused rather than padded. A short durability vector padded to length would
    /// report every slot past its end as indestructible, which is a lie nothing
    /// downstream could notice.
    #[test]
    fn misaligned_durability_vectors_are_protocol_errors() {
        for (name, durability, max_durability, want) in [
            (
                "a short durability vector",
                Some(&[0u16][..]),
                Some(&[0u16, 0][..]),
                DecodeError::DurabilityLength {
                    slots: 2,
                    durability: 1,
                    max_durability: 2,
                },
            ),
            (
                "a short maximum vector",
                Some(&[0u16, 0][..]),
                Some(&[0u16][..]),
                DecodeError::DurabilityLength {
                    slots: 2,
                    durability: 2,
                    max_durability: 1,
                },
            ),
            (
                "no durability vectors at all, which a V2 server would send",
                None,
                None,
                DecodeError::DurabilityLength {
                    slots: 2,
                    durability: 0,
                    max_durability: 0,
                },
            ),
        ] {
            assert_eq!(
                decode(&encode_inventory_state_with_durability(
                    Some(&[7, 1, 2, 64]),
                    durability,
                    max_durability,
                )),
                Err(want),
                "{name}"
            );
        }
    }

    #[test]
    fn impossible_slot_durability_is_a_protocol_error() {
        for (name, stacks, durability, max_durability, want) in [
            (
                "a current value with no maximum",
                &[7u16, 1][..],
                &[35u16][..],
                &[0u16][..],
                DecodeError::SlotDurability {
                    slot: 0,
                    item_id: 7,
                    count: 1,
                    durability: 35,
                    max_durability: 0,
                },
            ),
            (
                "more durability than the maximum",
                &[7, 1][..],
                &[101][..],
                &[100][..],
                DecodeError::SlotDurability {
                    slot: 0,
                    item_id: 7,
                    count: 1,
                    durability: 101,
                    max_durability: 100,
                },
            ),
            (
                "a durable stack, which is never a thing: one whole item, always",
                &[7, 2][..],
                &[100][..],
                &[100][..],
                DecodeError::SlotDurability {
                    slot: 0,
                    item_id: 7,
                    count: 2,
                    durability: 100,
                    max_durability: 100,
                },
            ),
            (
                "durability on an empty slot",
                &[0, 0][..],
                &[0][..],
                &[100][..],
                DecodeError::SlotDurability {
                    slot: 0,
                    item_id: 0,
                    count: 0,
                    durability: 0,
                    max_durability: 100,
                },
            ),
        ] {
            assert_eq!(
                decode(&encode_inventory_state_with_durability(
                    Some(stacks),
                    Some(durability),
                    Some(max_durability),
                )),
                Err(want),
                "{name}"
            );
        }
    }
}
