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

use std::collections::{HashMap, HashSet};
use std::fmt;

use flatbuffers::FlatBufferBuilder;

use crate::wire::voxelheim::net as fb;

/// Largest chunk edge the contract can address, mirroring
/// `protocol.MaxChunkSize`: a single RLE run length is a `u16`, and 40³ (64000)
/// is the last cube that fits.
pub const MAX_CHUNK_SIZE: u16 = 40;
/// Largest equipment tail this client will allocate from an untrusted welcome.
pub const MAX_EQUIPMENT_SLOTS: u8 = 8;

/// Bounds the streamed volume, which grows as `(2r + 1)³` chunks. Mirrors
/// `protocol.MaxViewDistance`.
pub const MAX_VIEW_DISTANCE: u8 = 16;

/// Maximum entries on either side of a player-trade offer; a decode bound, not UI layout.
pub const PLAYER_TRADE_SLOTS: usize = 5;

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
    /// Trailing inventory slots reserved for worn equipment. Guaranteed
    /// `1..=MAX_EQUIPMENT_SLOTS`,
    /// with hotbar and equipment together no larger than `inventory_slots`.
    pub equipment_slots: u8,
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
    /// How far a voice carries on this server, in blocks, or zero from a server that
    /// relays no voice at all. Guaranteed finite and not negative.
    ///
    /// Presentation, and the contract says so in as many words: the server recomputes
    /// the audible set from the positions it owns and sends a frame only to it, so
    /// receiving a [`VoiceHeard`] *is* the audibility answer. A client that filtered one
    /// by this radius would be re-deciding an outcome the only authority already
    /// settled, which is `view_distance`'s rule one message later.
    ///
    /// **Zero is a legal announcement rather than a missing value** — an operator who
    /// turned voice off — and it is the second number here allowed to be zero after
    /// validation, for [`WorldClock::day_length_ticks`]'s reason.
    pub voice_range_blocks: f32,
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

/// Which authoritative projectile presentation a [`ProjectileState`] uses.
///
/// No `Unknown` variant, for the reason [`LifeState`] has none. A zero or a kind
/// appended by a newer contract is not something this renderer can honestly draw,
/// so the decoder refuses it instead of inventing a replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectileKind {
    Arrow,
    EnergyOrb,
}

impl ProjectileKind {
    fn from_wire(value: fb::ProjectileKind) -> Option<Self> {
        match value {
            fb::ProjectileKind::Arrow => Some(Self::Arrow),
            fb::ProjectileKind::EnergyOrb => Some(Self::EnergyOrb),
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
    Deer,
    /// A settlement's resident. Never hostile, never lootable, never a corpse — see
    /// `MobKind.Villager` in `schemas/player.fbs`, which carries the argument.
    Villager,
    /// A capital paddock resident. It is routed to the shared horse rig and has no
    /// combat body on either side.
    Horse,
}

/// Which learned horse authoritative mount state names.
#[allow(
    clippy::enum_variant_names,
    reason = "the wire vocabulary spells the horse colour in each append-only member"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MountKind {
    BlackHorse,
    BrownHorse,
    GreyHorse,
}

impl MountKind {
    fn from_wire(value: fb::MountKind) -> Option<Self> {
        match value {
            fb::MountKind::BlackHorse => Some(Self::BlackHorse),
            fb::MountKind::BrownHorse => Some(Self::BrownHorse),
            fb::MountKind::GreyHorse => Some(Self::GreyHorse),
            _ => None,
        }
    }

    fn wire(self) -> fb::MountKind {
        match self {
            Self::BlackHorse => fb::MountKind::BlackHorse,
            Self::BrownHorse => fb::MountKind::BrownHorse,
            Self::GreyHorse => fb::MountKind::GreyHorse,
        }
    }
}

/// Which authoritative interruptible action is running for this recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastKind {
    Mount,
}

impl CastKind {
    fn from_wire(value: fb::CastKind) -> Option<Self> {
        match value {
            fb::CastKind::Mount => Some(Self::Mount),
            _ => None,
        }
    }
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
            fb::MobKind::Deer => Some(Self::Deer),
            fb::MobKind::Villager => Some(Self::Villager),
            fb::MobKind::Horse => Some(Self::Horse),
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
    Flee,
    Windup,
    Recovery,
    /// Killed, and going down. **The only statement of death this contract makes about a
    /// creature**, and the only thing a renderer may animate one on: a mob that simply
    /// leaves a snapshot may have been killed, may have walked out of the streamed cube,
    /// or may have been taken by the daylight, and nothing distinguishes those three from
    /// here. Zero `health` does not distinguish them either — it arrives *with* this
    /// action rather than instead of it.
    Dying,
    /// Dead, inert and retained as an authoritative loot container until expiry.
    Corpse,
}

impl MobAction {
    fn from_wire(value: fb::MobAction) -> Option<Self> {
        match value {
            fb::MobAction::Idle => Some(Self::Idle),
            fb::MobAction::Chase => Some(Self::Chase),
            fb::MobAction::Windup => Some(Self::Windup),
            fb::MobAction::Recovery => Some(Self::Recovery),
            fb::MobAction::Dying => Some(Self::Dying),
            fb::MobAction::Flee => Some(Self::Flee),
            fb::MobAction::Corpse => Some(Self::Corpse),
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
    Runestone,
}

impl StructureKind {
    /// The same rule [`MobKind::from_wire`] carries: a member is accepted here in the
    /// commit that teaches [`crate::player::structures`] to draw it, never before.
    fn from_wire(value: fb::StructureKind) -> Option<Self> {
        match value {
            fb::StructureKind::Tent => Some(Self::Tent),
            fb::StructureKind::Forge => Some(Self::Forge),
            fb::StructureKind::Campfire => Some(Self::Campfire),
            fb::StructureKind::Runestone => Some(Self::Runestone),
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

/// What one player entity looks like, sent as that player comes into view.
///
/// Cached against the entity id and resent only when the level changes. **Not part of a
/// snapshot, and that is the whole point**: `EntityState` is a struct inlined once per
/// visible entity per tick, and five colours plus one progression label would otherwise
/// be paid for at the tick rate for ever. See `schemas/player.fbs`.
///
/// An appearance for an entity this client has never seen is **not** an error: the two
/// streams are not ordered against each other, so either can arrive first and a
/// receiver holds whichever half it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerAppearance {
    pub entity_id: u64,
    pub appearance: Appearance,
    pub name: String,
    /// Server-derived current level. Guaranteed non-zero.
    pub level: u16,
    /// Item ids in the trailing equipment slots. Zero means nothing is worn there.
    pub worn_head: u16,
    pub worn_chest: u16,
    pub worn_legs: u16,
    pub worn_offhand: u16,
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
    CookedMeat,
    LeatherCap,
    LeatherJerkin,
    LeatherLeggings,
    IronHelm,
    IronCuirass,
    IronGreaves,
    WoodenShield,
    Bow,
    Arrows,
    WoodenSceptre,
    Runestone,
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
            Self::CookedMeat => fb::RecipeID::CookedMeat,
            Self::LeatherCap => fb::RecipeID::LeatherCap,
            Self::LeatherJerkin => fb::RecipeID::LeatherJerkin,
            Self::LeatherLeggings => fb::RecipeID::LeatherLeggings,
            Self::IronHelm => fb::RecipeID::IronHelm,
            Self::IronCuirass => fb::RecipeID::IronCuirass,
            Self::IronGreaves => fb::RecipeID::IronGreaves,
            Self::WoodenShield => fb::RecipeID::WoodenShield,
            Self::Bow => fb::RecipeID::Bow,
            Self::Arrows => fb::RecipeID::Arrows,
            Self::WoodenSceptre => fb::RecipeID::WoodenSceptre,
            Self::Runestone => fb::RecipeID::Runestone,
        }
    }
}

/// The recipient's own health, hunger, progression and life state, from the newest snapshot.
///
/// Replaces the previous value wholesale, exactly as an [`InventoryState`] does. There
/// is nothing to merge and nothing to advance locally: a dropped snapshot is harmless
/// because the next one carries the complete answer.
///
/// Every invariant `schemas/player.fbs` documents has already been checked by the time
/// one of these exists — health, hunger and experience denominators are non-zero and
/// no current value exceeds its maximum, so a presentation may divide without trusting
/// the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerVitals {
    /// Current health. Zero is legal and means dead — but it is `life_state` that says
    /// so, not this number.
    pub health: u16,
    /// Maximum health. Guaranteed non-zero.
    pub max_health: u16,
    /// Current hunger reserve. Zero is legal and stops server-side regeneration.
    pub hunger: u16,
    /// Maximum hunger. Guaranteed non-zero.
    pub max_hunger: u16,
    /// Current level. Guaranteed non-zero.
    pub level: u16,
    /// Experience earned into the current level. Never exceeds
    /// `experience_to_next`; equal to it at the level cap.
    pub experience: u32,
    /// Experience required to complete the current level. Guaranteed non-zero.
    pub experience_to_next: u32,
    pub life_state: LifeState,
    /// Server ticks until the server respawns this player, at
    /// [`SessionParams::tick_rate`]. Zero unless `life_state` is [`LifeState::Dead`].
    /// **A count, never a deadline**: converted for display, held unchanged while
    /// snapshots are absent, never run down from local time.
    pub respawn_ticks: u32,
    /// Whether the server is currently refusing damage to this player. The server owns
    /// the timer; this is its answer.
    pub invulnerable: bool,
    pub blocking: bool,
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
            hunger: 100,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state: LifeState::Alive,
            respawn_ticks: 0,
            invulnerable: false,
            blocking: false,
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
    /// The player this mob is hunting, or zero when it has no target.
    ///
    /// A non-zero id is deliberately not required to name an entity in this snapshot:
    /// the target may be outside this recipient's visibility while the mob remains in it.
    pub target_entity_id: u64,
}

/// One player named as mounted in this complete snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountState {
    pub entity_id: u64,
    pub mount: MountKind,
}

/// This recipient's own authoritative running cast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastState {
    pub kind: CastKind,
    pub progress: u8,
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

/// One projectile's authoritative state in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectileState {
    pub entity_id: u64,
    pub pos: [f32; 3],
    /// The newest server velocity is presentation input for heading and trail only.
    /// Interpolation never integrates or extrapolates it.
    pub vel: [f32; 3],
    pub kind: ProjectileKind,
}

/// One authoritative dropped item beside the player entities in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemDropState {
    pub entity_id: u64,
    pub pos: [f32; 3],
    pub item_id: u16,
    pub count: u16,
    /// Zero when this drop does not wear out. A non-zero maximum is copied from
    /// the server's sparse durability vector and always belongs to a count-one drop.
    pub durability: u16,
    pub max_durability: u16,
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
    /// Whether this campfire is burning.
    ///
    /// **It means something for [`StructureKind::Campfire`] and for nothing else.**
    /// `false` is a campfire the rain has put out; `true` is one that is alight. Every
    /// structure of every other kind carries `true` as well, and that is the contract
    /// field's default showing through rather than a statement about it — no server
    /// writes the byte for a tent or a forge, because neither is the sort of thing that
    /// burns. So `lit` on a non-campfire is "unset", not "burning", and a renderer that
    /// lit a structure on this field alone would set fire to every tent in the world:
    /// read the `kind` beside it first.
    ///
    /// **The server decides, and a doused fire is not a station** — it lights nothing,
    /// warms nobody and satisfies no recipe that needs a campfire nearby. Whether rain
    /// reaches this cell, how hard it falls and how long it takes are all authoritative.
    ///
    /// The contract's default is `true`, which is what lets a pre-V26 server's elided
    /// field read as the burning fire every fire on such a server is.
    pub lit: bool,
}

/// One other member of the snapshot recipient's party.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PartyMemberState {
    pub entity_id: u64,
    pub pos: [f32; 3],
    pub health: u16,
    pub max_health: u16,
    pub alive: bool,
}

/// One character in the complete stable party roster. `entity_id == 0` exactly while
/// offline; `character_id` and order survive reconnects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyRosterMember {
    pub character_id: u64,
    pub entity_id: u64,
    pub name: String,
    pub online: bool,
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
    /// Every projectile in view. The newest snapshot is the complete existence set.
    pub projectiles: Vec<ProjectileState>,
    /// Every mob in view. The newest snapshot is the **complete** set: a mob that stops
    /// appearing has stopped existing for this session, and the reason is never
    /// inferred from its health.
    pub mobs: Vec<MobState>,
    /// Sparse complete mount state, keyed to players in `entities`.
    pub mounts: Vec<MountState>,
    /// This client's own health and life state. Present in every snapshot by contract.
    pub self_vitals: PlayerVitals,
    /// The recipient's running cast; absence means no cast is running.
    pub self_cast: Option<CastState>,
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
    /// Persisted absolute world time from the same snapshot as `tick_of_day`.
    ///
    /// The session layer validates the pair against the day length announced in
    /// `ServerWelcome`, because this frame-only decoder has never seen that value.
    /// Unlike `server_tick`, this survives a server restart and is shared by every
    /// client connected to the world.
    pub world_tick: u64,
    /// Which of the players in `entities` the server currently holds dead.
    ///
    /// **A fact about the world rather than an event**, which is why it is a field of the
    /// snapshot and not a message: a session that joined after a death is told exactly what
    /// the session that watched it happen was told. Empty is the ordinary case.
    ///
    /// Two of the three invariants `schemas/player.fbs` attaches to it are enforced here —
    /// every id names a player in `entities`, and no id appears twice. The third is not, for
    /// the reason `tick_of_day` is not: "the recipient's own id is here exactly when
    /// `self_vitals` says `Dead`" names an entity id that arrived in the *welcome*, and
    /// [`decode`] sees one frame at a time. The handshake holds both halves and owns it.
    ///
    /// **Nothing in `player/` draws from this yet**, and that is a split rather than an
    /// oversight: this half lands the contract, the server that fills it and the decoder that
    /// refuses a frame breaking it. The half that tips a body on it follows.
    pub dead_players: Vec<u64>,
    pub blocking_players: Vec<u64>,
    /// Zero only when this session has no party. A non-zero value may name this
    /// session itself, so a frame-only decoder cannot require it to occur below.
    pub party_leader_entity_id: u64,
    /// Every other member of this session's party. The session layer that also owns
    /// `ServerWelcome.entity_id` must verify that the recipient itself is absent and
    /// that a leader not listed here is the recipient.
    pub party_members: Vec<PartyMemberState>,
    /// Complete authoritative party order, including this recipient and beginning with
    /// the leader. Unlike `party_members`, offline characters remain here.
    pub party_roster: Vec<PartyRosterMember>,
    /// Complete set of corpse containers this recipient may currently open.
    pub accessible_loot_corpses: Vec<u64>,
    /// What the sky is doing at **this recipient's own position**, this tick.
    ///
    /// **The third field here that is not about an entity**, and it rides along for the
    /// reason `tick_of_day` does: it changes every tick and is read by the same frame the
    /// world is drawn from, so a message of its own would arrive on its own schedule and
    /// put the rain a tick away from the ground it is falling on.
    ///
    /// `None` says this server keeps no weather at all, which a test world and a pre-V26
    /// server both legitimately are. That is the *field* being absent; a present
    /// [`WeatherState`] naming an unknown kind is a protocol error and refused.
    ///
    /// **Nothing in `player/` draws from this yet**, and that is a split rather than an
    /// oversight — the same one `dead_players` had. This half lands the decoder that
    /// copies and validates it; the precipitation volume is #466 and the storm's own
    /// countdown is #470.
    pub weather: Option<WeatherState>,
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
            projectiles: Vec::new(),
            mobs: Vec::new(),
            mounts: Vec::new(),
            self_vitals: PlayerVitals::unharmed(),
            self_cast: None,
            structures: Vec::new(),
            tick_of_day: 0,
            world_tick: 0,
            dead_players: Vec::new(),
            blocking_players: Vec::new(),
            party_leader_entity_id: 0,
            party_members: Vec::new(),
            party_roster: Vec::new(),
            accessible_loot_corpses: Vec::new(),
            weather: None,
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
    pub silver: u32,
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
    Chat,
    Party,
    OpenLoot,
    TakeLoot,
    Attack,
    RequestMapTile,
    PlaceMarker,
    RemoveMarker,
    Interact,
    Trade,
    /// A `BlockEditRequest` the server would not apply. V26, and the member warded ground
    /// answers an edit with.
    Edit,
    /// A `MineRequest` the server would not begin or finish. Distinct from
    /// [`Self::MineBlock`], which names the same action and is the value a shipped server
    /// may already have sent: both stay, and a receiver names both.
    Mine,
    /// Beginning or interrupting the authoritative mount cast.
    Mount,
    /// A player trade refusal, distinct from vendor [`Self::Trade`].
    PlayerTrade,
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
            fb::RefusedAction::Chat => Self::Chat,
            fb::RefusedAction::Party => Self::Party,
            fb::RefusedAction::OpenLoot => Self::OpenLoot,
            fb::RefusedAction::TakeLoot => Self::TakeLoot,
            fb::RefusedAction::Attack => Self::Attack,
            fb::RefusedAction::RequestMapTile => Self::RequestMapTile,
            fb::RefusedAction::PlaceMarker => Self::PlaceMarker,
            fb::RefusedAction::RemoveMarker => Self::RemoveMarker,
            fb::RefusedAction::Interact => Self::Interact,
            fb::RefusedAction::Trade => Self::Trade,
            fb::RefusedAction::Edit => Self::Edit,
            fb::RefusedAction::Mine => Self::Mine,
            fb::RefusedAction::Mount => Self::Mount,
            fb::RefusedAction::PlayerTrade => Self::PlayerTrade,
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
    TooFast,
    PartyFull,
    NoSuchPlayer,
    AlreadyInParty,
    NoInvite,
    NotLeader,
    CorpseUnavailable,
    LootNotOwned,
    StaleRevision,
    InventoryFull,
    NoAmmunition,
    TileMisaligned,
    TooManyMarkers,
    NoteTooLong,
    MarkerUnknown,
    NotAVendor,
    NotEnoughSilver,
    VendorDoesNotWant,
    /// This ground is under a runestone that is not the player's, or under a settlement's
    /// own ward. In the low group: the request was legal and the world said no.
    ///
    /// It names no owner, deliberately — the argument `MarkerUnknown` records, one field
    /// over: an answer that did would let a client learn who has claimed ground by poking
    /// at it.
    Warded,
    MountNotLearned,
    AlreadyMounted,
    MountNotGrounded,
    MountIndoors,
    MountLowCeiling,
    CastAlreadyInProgress,
    CastInterruptedByDamage,
    CastInterruptedByMovement,
    CastInterruptedByJump,
    CastInterruptedByDeath,
    ActionForbiddenWhileMounted,
    MountAlreadyLearned,
    AlreadyTrading,
    TradeNotOpen,
    TradeSlotTaken,
    NothingToOffer,
    TradeCooldown,

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
            fb::RefusalReason::TooFast => Self::TooFast,
            fb::RefusalReason::PartyFull => Self::PartyFull,
            fb::RefusalReason::NoSuchPlayer => Self::NoSuchPlayer,
            fb::RefusalReason::AlreadyInParty => Self::AlreadyInParty,
            fb::RefusalReason::NoInvite => Self::NoInvite,
            fb::RefusalReason::NotLeader => Self::NotLeader,
            fb::RefusalReason::CorpseUnavailable => Self::CorpseUnavailable,
            fb::RefusalReason::LootNotOwned => Self::LootNotOwned,
            fb::RefusalReason::StaleRevision => Self::StaleRevision,
            fb::RefusalReason::InventoryFull => Self::InventoryFull,
            fb::RefusalReason::NoAmmunition => Self::NoAmmunition,
            fb::RefusalReason::TileMisaligned => Self::TileMisaligned,
            fb::RefusalReason::TooManyMarkers => Self::TooManyMarkers,
            fb::RefusalReason::NoteTooLong => Self::NoteTooLong,
            fb::RefusalReason::MarkerUnknown => Self::MarkerUnknown,
            fb::RefusalReason::NotAVendor => Self::NotAVendor,
            fb::RefusalReason::NotEnoughSilver => Self::NotEnoughSilver,
            fb::RefusalReason::VendorDoesNotWant => Self::VendorDoesNotWant,
            fb::RefusalReason::Warded => Self::Warded,
            fb::RefusalReason::MountNotLearned => Self::MountNotLearned,
            fb::RefusalReason::AlreadyMounted => Self::AlreadyMounted,
            fb::RefusalReason::MountNotGrounded => Self::MountNotGrounded,
            fb::RefusalReason::MountIndoors => Self::MountIndoors,
            fb::RefusalReason::MountLowCeiling => Self::MountLowCeiling,
            fb::RefusalReason::CastAlreadyInProgress => Self::CastAlreadyInProgress,
            fb::RefusalReason::CastInterruptedByDamage => Self::CastInterruptedByDamage,
            fb::RefusalReason::CastInterruptedByMovement => Self::CastInterruptedByMovement,
            fb::RefusalReason::CastInterruptedByJump => Self::CastInterruptedByJump,
            fb::RefusalReason::CastInterruptedByDeath => Self::CastInterruptedByDeath,
            fb::RefusalReason::ActionForbiddenWhileMounted => Self::ActionForbiddenWhileMounted,
            fb::RefusalReason::MountAlreadyLearned => Self::MountAlreadyLearned,
            fb::RefusalReason::AlreadyTrading => Self::AlreadyTrading,
            fb::RefusalReason::TradeNotOpen => Self::TradeNotOpen,
            fb::RefusalReason::TradeSlotTaken => Self::TradeSlotTaken,
            fb::RefusalReason::NothingToOffer => Self::NothingToOffer,
            fb::RefusalReason::TradeCooldown => Self::TradeCooldown,
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

/// Starts or releases the held shield; the server validates the intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRequest {
    pub active: bool,
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

/// Intent to consume one item from one authoritative inventory slot.
///
/// The item id, the amount restored and whether the request succeeds are deliberately
/// absent: the server reads all three from its own inventory and item registry. The slot
/// travels verbatim and an unusable one is an ordinary silent gameplay refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumeRequest {
    /// The authoritative inventory slot whose item the player is trying to consume.
    pub slot: u16,
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

/// Intent to put one whole stack back on the ground. One slot index and nothing else.
///
/// There is no count and no position, and both absences are the safety: a count would let
/// this client state what leaves its own pack, and a position would let it put an item down
/// anywhere in the world. What the slot holds and where the player's feet are is read
/// server-side, so the index travels verbatim and is refused by the simulation rather than
/// by the framing.
///
/// A refused drop is silence. An accepted one arrives as the complete `InventoryState` that
/// follows and as an `ItemDropState` in the next snapshot — indistinguishable there from a
/// drop the world produced, which is the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropItemRequest {
    /// The authoritative inventory slot to empty onto the ground.
    pub slot: u8,
    /// This client's own tick counter — the same one `PlayerInput` uses.
    pub client_tick: u32,
}

/// The server-owned linger window acknowledged after a leave request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaveStarted {
    /// Remaining server time when the acknowledgement was produced.
    pub remaining_ms: u32,
}

/// The authoritative answer to one request to stop leaving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaveCancelResult {
    /// Only this value may put the client back in play.
    pub accepted: bool,
    /// The server-owned time still remaining when cancellation was refused.
    pub remaining_ms: u32,
}

/// One world-chat line this client asks the authoritative server to accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    /// Display text copied verbatim. Acceptance belongs to the server.
    pub text: String,
}

/// One accepted world-chat line from the authoritative server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub sender_entity_id: u64,
    /// Display text. Shown, never parsed or used as identity.
    pub sender_name: String,
    /// Display text copied verbatim, including the empty string.
    pub text: String,
}

/// Which party operation a client asks the authoritative server to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartyAction {
    Invite,
    Accept,
    Decline,
    Leave,
    Kick,
}

impl PartyAction {
    fn wire(self) -> fb::PartyAction {
        match self {
            Self::Invite => fb::PartyAction::Invite,
            Self::Accept => fb::PartyAction::Accept,
            Self::Decline => fb::PartyAction::Decline,
            Self::Leave => fb::PartyAction::Leave,
            Self::Kick => fb::PartyAction::Kick,
        }
    }
}

/// One client intent to change party membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyRequest {
    pub action: PartyAction,
    /// Display text copied verbatim. Read by the server only for Invite and Kick.
    pub target_name: String,
}

/// One still-live party invitation from the authoritative server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyInvite {
    pub from_entity_id: u64,
    /// Display text. Shown, never parsed or used as identity.
    pub from_name: String,
    pub expires_ms: u32,
}

/// Intent to open one server-owned corpse container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootOpenRequest {
    pub corpse_id: u64,
    pub client_tick: u32,
}

/// Intent to move one stable entry from one authoritative revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootTakeRequest {
    pub corpse_id: u64,
    pub entry_id: u64,
    pub revision: u32,
    pub client_tick: u32,
}

/// Intent to move every entry of one authoritative revision that fits.
///
/// It names no entry and carries no count, because neither is this side's to decide: the
/// server owns the order it walks the container in and whether each stack fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootTakeAllRequest {
    pub corpse_id: u64,
    pub revision: u32,
    pub client_tick: u32,
}

/// One authoritative stack in a corpse container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootEntry {
    pub entry_id: u64,
    pub item_id: u16,
    pub count: u16,
    pub durability: u16,
    pub max_durability: u16,
}

/// Complete per-recipient contents for one corpse revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LootState {
    pub corpse_id: u64,
    pub revision: u32,
    pub entries: Vec<LootEntry>,
    pub silver: u32,
}

/// Explicit end of one open corpse container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootClosed {
    pub corpse_id: u64,
}

/// Complete authoritative learned-mount set for this recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnedMounts {
    pub mounts: Vec<MountKind>,
}

/// Intent to begin calling one learned mount. Legality and outcome stay server-owned.
#[allow(
    dead_code,
    reason = "the V27 request is reserved before its mount-menu consumer"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountRequest {
    pub mount: MountKind,
}

/// The fixed pixel edge of every map tile, at every scale.
///
/// Fixing the pixel count rather than the block span is what keeps a tile's arrays a
/// constant size: this side never allocates from a number the server chose.
pub const MAP_TILE_EDGE: usize = 64;

/// The entry count of [`MapTile::height`] and [`MapTile::surface`].
pub const MAP_TILE_CELLS: usize = MAP_TILE_EDGE * MAP_TILE_EDGE;

/// The block edge of one chunk column, which is the granularity the server records
/// exploration at and therefore the granularity [`MapTile::explored`] is a mask over.
pub const CHUNK_COLUMN_BLOCKS: i32 = 32;

/// The most chunk columns one [`MapExplored`] page may carry.
pub const MAX_EXPLORED_COLUMNS: usize = 4096;

/// The most marks one character may hold.
pub const MAX_MARKERS: usize = 64;

/// The most bytes a mark's note may carry. Bytes rather than characters, because a byte
/// is what the wire carries and what both decoders can count without agreeing on an
/// encoding of characters.
pub const MARKER_NOTE_MAX_BYTES: usize = 120;

/// The most bytes a resident's name may carry. Bytes rather than characters, for the
/// reason [`MARKER_NOTE_MAX_BYTES`] is counted in them.
pub const RESIDENT_NAME_MAX_BYTES: usize = 32;

/// The only blocks-per-pixel values this contract has.
///
/// A scale is a member of a fixed set rather than a range, so the absent-field zero
/// fails closed along with everything else.
pub const MAP_TILE_SCALES: [u8; 3] = [1, 4, 16];

/// How many blocks a tile covers on each axis at `scale`, and therefore the grid every
/// tile origin sits on. `None` for a scale this contract has no member for.
pub fn map_tile_span(scale: u8) -> Option<i32> {
    MAP_TILE_SCALES
        .contains(&scale)
        .then(|| MAP_TILE_EDGE as i32 * i32::from(scale))
}

/// The exact byte length of [`MapTile::explored`] at `scale`.
///
/// The tile covers `(span / CHUNK_COLUMN_BLOCKS)²` chunk columns, one bit each, rounded
/// up to whole bytes: 1, 8 and 128. Scale 1 is the one case that rounds, and its four
/// unused high bits are zero.
pub fn map_tile_explored_bytes(scale: u8) -> Option<usize> {
    let span = map_tile_span(scale)?;
    let edge = (span / CHUNK_COLUMN_BLOCKS) as usize;
    Some(edge.pow(2).div_ceil(8))
}

/// The *kind* of what one map pixel shows from above. Deliberately not a block id.
///
/// `schemas/world.fbs` carries the argument: a map draws what a place is, and `Forest`,
/// `Cave` and `Settlement` are none of them a block. `Unknown` **is** a variant here,
/// unlike [`EditAction`]'s missing one, because this side receives it as a real value:
/// it is what every pixel of an unexplored chunk column carries, and drawing nothing for
/// it is the whole of what the mask is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapSurface {
    /// Nothing is known about this pixel, or nothing may be said about it.
    Unknown,
    Grass,
    Snow,
    Sand,
    Stone,
    Gravel,
    Water,
    Ice,
    Forest,
    Cave,
    Settlement,
}

impl MapSurface {
    /// Partial, unlike [`RefusalReason::from_wire`], and the difference is what the
    /// value costs when it cannot be read. A refusal nobody can name loses one sentence;
    /// a surface nobody can name would be drawn, and a map with a guessed colour on it is
    /// worse than no map. `Settlement` is reserved in the contract precisely so that this
    /// stays a refusal rather than becoming a routine forward-compatibility case.
    fn from_wire(value: fb::MapSurface) -> Option<Self> {
        match value {
            fb::MapSurface::Unknown => Some(Self::Unknown),
            fb::MapSurface::Grass => Some(Self::Grass),
            fb::MapSurface::Snow => Some(Self::Snow),
            fb::MapSurface::Sand => Some(Self::Sand),
            fb::MapSurface::Stone => Some(Self::Stone),
            fb::MapSurface::Gravel => Some(Self::Gravel),
            fb::MapSurface::Water => Some(Self::Water),
            fb::MapSurface::Ice => Some(Self::Ice),
            fb::MapSurface::Forest => Some(Self::Forest),
            fb::MapSurface::Cave => Some(Self::Cave),
            fb::MapSurface::Settlement => Some(Self::Settlement),
            _ => None,
        }
    }
}

/// What a mark on the map means.
///
/// The wire's `Unknown = 0` is deliberately **not** a variant, for the reason
/// [`EditAction`]'s is not: it exists so a request that omits `kind` fails closed on the
/// server, and having it here would only be a value every encoder had to promise never to
/// send. It is refused in the other direction too — a `Marker` carrying it is a protocol
/// error rather than a pin nobody can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    Resource,
    Cave,
    Monster,
    Boss,
    Camp,
    Village,
    /// A mark that is only its note.
    Note,
}

impl MarkerKind {
    /// Every kind, in the order `schemas/player.fbs` declares them.
    ///
    /// One list, so the row of kind buttons on the map is laid out from the contract rather
    /// than from a second enumeration somebody has to remember to extend.
    pub const ALL: [Self; 7] = [
        Self::Resource,
        Self::Cave,
        Self::Monster,
        Self::Boss,
        Self::Camp,
        Self::Village,
        Self::Note,
    ];

    fn from_wire(value: fb::MarkerKind) -> Option<Self> {
        match value {
            fb::MarkerKind::Resource => Some(Self::Resource),
            fb::MarkerKind::Cave => Some(Self::Cave),
            fb::MarkerKind::Monster => Some(Self::Monster),
            fb::MarkerKind::Boss => Some(Self::Boss),
            fb::MarkerKind::Camp => Some(Self::Camp),
            fb::MarkerKind::Village => Some(Self::Village),
            fb::MarkerKind::Note => Some(Self::Note),
            _ => None,
        }
    }

    /// The wire value. Total, and that is the point of the missing `Unknown`.
    fn wire(self) -> fb::MarkerKind {
        match self {
            Self::Resource => fb::MarkerKind::Resource,
            Self::Cave => fb::MarkerKind::Cave,
            Self::Monster => fb::MarkerKind::Monster,
            Self::Boss => fb::MarkerKind::Boss,
            Self::Camp => fb::MarkerKind::Camp,
            Self::Village => fb::MarkerKind::Village,
            Self::Note => fb::MarkerKind::Note,
        }
    }
}

/// One square of the map this client is asking for. **A request for data, never an
/// outcome**, exactly as `ChunkResendRequest` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapTileRequest {
    /// Block x of the tile's minimum corner. A multiple of `64 * scale`.
    pub origin_x: i32,
    /// Block z of the tile's minimum corner.
    pub origin_z: i32,
    /// Blocks per pixel: 1, 4 or 16.
    pub scale: u8,
    /// Ordering and staleness only. The server never reads it as a clock.
    pub client_tick: u32,
}

/// One 64 x 64 square of the map, as the server drew it for this character.
///
/// `height` and `surface` are [`MAP_TILE_CELLS`] entries each, row-major with z outer
/// and x inner. A pixel whose chunk column is not set in `explored` carries `0` and
/// [`MapSurface::Unknown`] — the server never sends the shape of the unexplored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapTile {
    pub origin_x: i32,
    pub origin_z: i32,
    pub scale: u8,
    /// Biased surface heights, `clamp(y + 64, 0, 255)`. Shading, never a coordinate.
    pub height: Vec<u8>,
    /// What each pixel is, in the same order as `height`.
    pub surface: Vec<MapSurface>,
    /// One bit per chunk column the tile covers, row-major and LSB first.
    pub explored: Vec<u8>,
}

/// One chunk column's address on the horizontal plane. No `cy`: a character who has been
/// somewhere has been there at every height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MapColumn {
    pub cx: i32,
    pub cz: i32,
}

/// One page of the additive ledger of where this character has been.
///
/// Additive is the whole protocol: a column named once is explored for good, so the
/// client's ledger is the union of every page it has received and no page is ever the
/// last one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapExplored {
    pub columns: Vec<MapColumn>,
}

/// One mark this client asks to put on its map. It names no id, because identity is the
/// server's to mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerPlaceRequest {
    pub x: i32,
    pub z: i32,
    pub kind: MarkerKind,
    /// At most [`MARKER_NOTE_MAX_BYTES`] bytes. Empty is ordinary.
    pub note: String,
    pub client_tick: u32,
}

/// One of this character's own marks, asked to come off the map. There is no edit
/// message: a change is a removal and a placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkerRemoveRequest {
    pub marker_id: u64,
    pub client_tick: u32,
}

/// One authoritative mark on this character's map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// Server-minted, stable for the life of the mark, unique within the character.
    pub marker_id: u64,
    pub x: i32,
    pub z: i32,
    pub kind: MarkerKind,
    pub note: String,
}

/// Every mark this character holds, **replacing** the client's copy wholesale.
///
/// An empty list is meaningful and ordinary — a character who has marked nothing — which
/// is why this is not the message whose empty case is refused. [`MapExplored`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerList {
    pub markers: Vec<Marker>,
}

/// What a resident does in their settlement.
///
/// No `Unknown` variant, for the reason [`MobKind`] has none: the contract's zero is the
/// absent field, and a resident whose role failed to arrive is refused rather than drawn
/// as a generic villager.
///
/// **A role is not a capability.** `Trader` does not mean this entity has a stall. What
/// opens when a resident is addressed is the server's answer and is never inferred here —
/// see `ResidentRole` in `schemas/player.fbs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentRole {
    Villager,
    Smith,
    Carpenter,
    Cook,
    Trader,
    Guard,
    Stablemaster,
}

impl ResidentRole {
    /// `None` for the contract's zero and for a member this build has no name for, which
    /// are the same refusal: [`decode`]'s caller ends the session over either.
    fn from_wire(value: fb::ResidentRole) -> Option<Self> {
        match value {
            fb::ResidentRole::Villager => Some(Self::Villager),
            fb::ResidentRole::Smith => Some(Self::Smith),
            fb::ResidentRole::Carpenter => Some(Self::Carpenter),
            fb::ResidentRole::Cook => Some(Self::Cook),
            fb::ResidentRole::Trader => Some(Self::Trader),
            fb::ResidentRole::Guard => Some(Self::Guard),
            fb::ResidentRole::Stablemaster => Some(Self::Stablemaster),
            _ => None,
        }
    }
}

/// What one resident is called and what they do, sent once as the entity enters view.
///
/// The [`PlayerAppearance`] precedent exactly: static per entity, cached against the
/// entity id, and not ordered against the snapshot stream — either may arrive first, and
/// neither order is an error.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidentAppearance {
    pub entity_id: u64,
    /// Display text, at most [`RESIDENT_NAME_MAX_BYTES`] bytes. Never parsed and never
    /// used as identity, which is what `entity_id` is for.
    pub name: String,
    pub role: ResidentRole,
    pub appearance: Appearance,
}

/// One line of a stall's price list: what is traded and the silver it costs per unit.
///
/// There is no stock count, because stock is unlimited by contract. A client that showed
/// one would be inventing a number the server never sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorEntry {
    pub item_id: u16,
    /// Silver per unit. The direction is the vector this entry came from, never a sign.
    pub price: u16,
}

/// The complete price list one vendor shows this recipient, **replacing** the client's
/// previous view of that vendor wholesale.
///
/// Revisioned like [`LootState`] and acted against the same way: a [`TradeRequest`] names
/// the revision it was written for, and the server refuses one written against a list it
/// has since replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorState {
    pub entity_id: u64,
    pub revision: u32,
    /// What the player may buy, at the price they pay.
    pub sells: Vec<VendorEntry>,
    /// What the vendor buys, at the price it pays.
    pub buys: Vec<VendorEntry>,
}

/// The named stall is no longer open. The client closes presentation and infers no
/// reason — [`LootClosed`]'s shape, and the same silence about why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorClosed {
    pub entity_id: u64,
}

/// "I address this resident." It states nothing: whether the player is close enough,
/// whether that entity keeps a stall and whether anything opens are the server's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcInteractRequest {
    pub entity_id: u64,
    pub client_tick: u32,
}

/// One intent to trade with a vendor: an item, a count, a direction, and the revision the
/// player was looking at.
///
/// **No price and no total**, deliberately. Both belong to the server, and a request that
/// named either would be this client stating an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeRequest {
    pub entity_id: u64,
    pub item_id: u16,
    /// Never zero. All or nothing: a request for five that only three can be afforded of
    /// buys none.
    pub count: u16,
    /// True when the player is buying from [`VendorState::sells`], false when they are
    /// selling into [`VendorState::buys`].
    pub buying: bool,
    pub revision: u32,
    pub client_tick: u32,
}

/// Which operation one [`PlayerTradeRequest`] asks the server to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerTradeAction {
    #[allow(
        dead_code,
        reason = "part 3 of #750 originates Open from the local prompt"
    )]
    Open,
    SetItem,
    ClearItem,
    SetSilver,
    Confirm,
    Cancel,
}

impl PlayerTradeAction {
    fn wire(self) -> fb::PlayerTradeAction {
        match self {
            Self::Open => fb::PlayerTradeAction::Open,
            Self::SetItem => fb::PlayerTradeAction::SetItem,
            Self::ClearItem => fb::PlayerTradeAction::ClearItem,
            Self::SetSilver => fb::PlayerTradeAction::SetSilver,
            Self::Confirm => fb::PlayerTradeAction::Confirm,
            Self::Cancel => fb::PlayerTradeAction::Cancel,
        }
    }
}

/// Player-trade intent. It names a pack slot; the server resolves that slot's stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerTradeRequest {
    pub action: PlayerTradeAction,
    pub target_entity_id: u64,
    pub trade_slot: u8,
    pub pack_slot: u8,
    pub silver: u32,
    pub revision: u32,
    pub client_tick: u32,
}

/// One authoritative stack; `pack_slot` is meaningful only in `my_offer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerTradeSlot {
    pub trade_slot: u8,
    pub pack_slot: u8,
    pub item_id: u16,
    pub count: u16,
    pub durability: u16,
    pub max_durability: u16,
}

/// One recipient's complete revisioned view; a later value replaces it wholesale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerTradeState {
    pub partner_entity_id: u64,
    pub partner_name: String,
    pub revision: u32,
    pub my_offer: Vec<PlayerTradeSlot>,
    pub their_offer: Vec<PlayerTradeSlot>,
    pub my_silver: u32,
    pub their_silver: u32,
    pub my_confirmed: bool,
    pub their_confirmed: bool,
}

/// Why a trade ended. Absent and newer wire members become `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerTradeCloseReason {
    Unknown,
    Completed,
    Cancelled,
    OutOfReach,
    Died,
    Disconnected,
    Failed,
}

impl PlayerTradeCloseReason {
    fn from_wire(value: fb::PlayerTradeCloseReason) -> Self {
        match value {
            fb::PlayerTradeCloseReason::Completed => Self::Completed,
            fb::PlayerTradeCloseReason::Cancelled => Self::Cancelled,
            fb::PlayerTradeCloseReason::OutOfReach => Self::OutOfReach,
            fb::PlayerTradeCloseReason::Died => Self::Died,
            fb::PlayerTradeCloseReason::Disconnected => Self::Disconnected,
            fb::PlayerTradeCloseReason::Failed => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

/// Explicit termination of the trade with one partner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerTradeClosed {
    pub partner_entity_id: u64,
    pub reason: PlayerTradeCloseReason,
}

/// What the sky is doing where the player stands.
///
/// No `Unknown` variant, for the reason [`ResidentRole`] has none — with one difference
/// worth stating, because it is what makes the refusal unambiguous here. [`WeatherState`]
/// is a *struct*, so "this server keeps no weather" is already representable as the whole
/// field being absent. A struct that is present and names zero is therefore a defect
/// rather than a silence, and [`Self::from_wire`] answers `None` for it.
///
/// Members are appended, never inserted, mirroring `WeatherKind` in `schemas/player.fbs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherKind {
    Clear,
    Rain,
    Snow,
    Sandstorm,
    /// The storm's own kind. It arrives only while a [`StormWarning`] says
    /// [`StormPhase::Raging`]; the ordinary weather of a climate never produces it.
    /// **Both halves of that statement are the server's**, and this side checks neither
    /// against the other: the two travel in different messages and `decode` sees one
    /// frame at a time, so a client that refused a `Blizzard` for want of a warning it
    /// had not been sent yet would be inventing an ordering the contract does not state.
    Blizzard,
}

impl WeatherKind {
    /// `None` for the contract's zero and for a member this build has no name for, which
    /// are one refusal: [`decode`]'s caller ends the session over either.
    fn from_wire(value: fb::WeatherKind) -> Option<Self> {
        match value {
            fb::WeatherKind::Clear => Some(Self::Clear),
            fb::WeatherKind::Rain => Some(Self::Rain),
            fb::WeatherKind::Snow => Some(Self::Snow),
            fb::WeatherKind::Sandstorm => Some(Self::Sandstorm),
            fb::WeatherKind::Blizzard => Some(Self::Blizzard),
            _ => None,
        }
    }
}

/// The weather at the recipient's own position, for one tick.
///
/// **Authoritative for what it does and presentation for what is drawn from it**, and the
/// split runs through the middle of the value. The server has already applied the cold,
/// the slowed step and the doused fire, and those outcomes arrive as vitals, as position
/// and as [`StructureState::lit`]. What is left for this side is particles, fog, wind and
/// sound. A client that re-derived an effect from these two bytes would be deciding a
/// gameplay outcome from presentation data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeatherState {
    pub kind: WeatherKind,
    /// How hard it is coming down: 0 is none, 255 is the most the sky can do. Not a
    /// percentage and not a rate. Nothing divides the range into named bands, so a
    /// consumer interpolates rather than switching on it.
    ///
    /// [`WeatherKind::Clear`] always carries 0, and a `Clear` that carries anything else
    /// is refused rather than clamped: the two halves of the struct would then be
    /// describing different skies, and neither reading is the server's.
    pub intensity: u8,
}

/// Where a blizzard is in its life, in a [`StormWarning`].
///
/// No `Unknown` variant, and it matters more here than anywhere else this rule is
/// applied: the phase is what gives [`StormWarning::seconds_until`] its meaning. A
/// countdown, a remaining duration and a zero are three different numbers wearing one
/// field, so a phase that failed to decode would pick the wrong one silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StormPhase {
    /// Decided and not yet arrived. `seconds_until` counts down to it.
    Approaching,
    /// Here. `seconds_until` is what it has left rather than what it is waiting for.
    Raging,
    /// Over, and `seconds_until` is 0.
    Passed,
}

impl StormPhase {
    /// `None` for the contract's zero and for a member this build has no name for.
    fn from_wire(value: fb::StormPhase) -> Option<Self> {
        match value {
            fb::StormPhase::Approaching => Some(Self::Approaching),
            fb::StormPhase::Raging => Some(Self::Raging),
            fb::StormPhase::Passed => Some(Self::Passed),
            _ => None,
        }
    }
}

/// The server telling this session about the blizzard.
///
/// **It carries no weather.** What the sky is doing where the player stands arrives in
/// the snapshot, every tick, as [`Snapshot::weather`]. This message says only that a
/// storm is coming, is here, or is done — and a client that inferred
/// [`WeatherKind::Blizzard`] from a [`StormPhase::Raging`] would be drawing weather the
/// server had not stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StormWarning {
    /// Seconds until the storm arrives while [`StormPhase::Approaching`], seconds it has
    /// left while [`StormPhase::Raging`], and 0 once it has [`StormPhase::Passed`].
    ///
    /// A duration in whole seconds, never a tick count and never a timestamp, and
    /// unbounded above by this contract.
    pub seconds_until: u32,
    pub phase: StormPhase,
}

/// The most warded chunk columns one [`WardsNearby`] may carry.
///
/// Read off the contract rather than written again here, unlike [`MAX_MARKERS`] and the
/// two byte bounds beside it: `schemas/player.fbs` states this one as a `WardBound` enum
/// precisely so both generated trees carry it and neither consumer keeps a copy that could
/// drift. 2048 is a square 45 chunks on a side, past any view distance a welcome may
/// announce, so a legal server never approaches it — it exists because this side allocates
/// from a length the peer chose.
pub const MAX_WARDED_COLUMNS: usize = fb::WardBound::MaxWardedColumns.0 as usize;

/// The most Opus bytes one [`VoiceHeard`] may carry.
///
/// Read off the contract for [`MAX_WARDED_COLUMNS`]'s reason: `schemas/player.fbs`
/// states it as a `VoiceBound` enum so the server that refuses an oversized frame and
/// the client that sizes a buffer from a relayed one read one constant rather than two
/// copies of a paragraph. 400 bytes is a 20 ms frame at 160 kbit/s, comfortably above
/// what a wideband stream needs and below anything a single frame has a reason to be.
pub const MAX_OPUS_BYTES: usize = fb::VoiceBound::MaxOpusBytes.0 as usize;

/// What put a ward on a chunk column.
///
/// No `Unknown` variant, and refused rather than drawn as a generic claim: shading a
/// column whose ward the client cannot name would tell the player a settlement owns
/// ground a runestone does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WardKind {
    /// A runestone somebody planted. [`WardedColumn::mine`] says whether it was this
    /// player.
    Runestone,
    /// A settlement's own ground, which belongs to nobody and is never `mine`.
    Settlement,
}

impl WardKind {
    /// `None` for the contract's zero and for a member this build has no name for.
    fn from_wire(value: fb::WardKind) -> Option<Self> {
        match value {
            fb::WardKind::Runestone => Some(Self::Runestone),
            fb::WardKind::Settlement => Some(Self::Settlement),
            _ => None,
        }
    }
}

/// One chunk column under a ward.
///
/// Only the horizontal axes, deliberately: a ward claims a column of the world from
/// bedrock to sky rather than a cube of it, so there is no `cy` to carry and none to
/// ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WardedColumn {
    pub cx: i32,
    pub cz: i32,
    pub kind: WardKind,
    /// True when the recipient is the one whose ward this is. **Presentation only** — the
    /// server refuses the edit either way, and a client that let a `true` here authorise
    /// anything would be deciding a gameplay outcome.
    pub mine: bool,
}

/// Every warded chunk column within the recipient's view, **replacing** the client's
/// previous set wholesale.
///
/// [`MarkerList`]'s shape rather than [`MapExplored`]'s, and the difference is the point:
/// this is a complete statement of a fact that changes, not a page of an additive ledger.
/// **An empty set is legal and ordinary** — it is how a client learns it has walked out of
/// the last ward — which is why absence and empty mean the same thing here, where an empty
/// `MapExplored` page is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WardsNearby {
    pub columns: Vec<WardedColumn>,
}

/// One Opus frame the authoritative server decided this session may hear.
///
/// **Receiving one is the audibility decision**, already made. Nothing here asks whether
/// the speaker is close enough or in the right party: the server owns both positions and
/// both party rosters, and a client that filtered on its own idea of either would be
/// second-guessing the authority that answered by sending the frame.
///
/// The bytes are copied out and never read here. Whether they are a legal Opus frame is
/// the decoder's question in #851, and this module owns the envelope — the same split
/// `ChunkData`'s runs are decoded under. **They are also never logged, never persisted
/// and never quoted in a diagnostic**, on either side of the wire: a voice frame is
/// personal data, which is why [`DecodeError`] names lengths and speakers and no
/// refusal here can print a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceHeard {
    /// The speaker's live entity, the same id an `EntitySnapshot` addresses them by.
    /// Guaranteed non-zero.
    pub speaker_entity_id: u64,
    /// The speaker's own monotonic counter, copied through unchanged so a listener can
    /// order frames and hear a gap as a gap. Per speaker: two speakers' sequences say
    /// nothing about each other. Presentation, never a clock — no gameplay branch reads
    /// it, and nothing here rejects a frame for arriving out of order.
    pub sequence: u32,
    /// The encoded frame, verbatim. Guaranteed non-empty and at most
    /// [`MAX_OPUS_BYTES`].
    pub opus: Vec<u8>,
}

/// One authoritative monster blow that reduced this player's health.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MobHit {
    pub attacker_entity_id: u64,
    pub attacker_pos: [f32; 3],
}

/// A decoded `ServerReject`.
///
/// [`Self::describe`] is the one place the code and the detail become a single
/// string. It has one owner deliberately: the ECS boundary produces that string and
/// `ui/status.rs` reads the code back out of it, so a separator chosen in two places
/// would let the two drift apart with every test still green. The typed value crosses
/// the net-thread boundary intact: a character-name refusal is the one rejection whose
/// code changes what the client can offer next, and turning it into display text before
/// that decision is exactly how the distinction used to be lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject {
    /// The schema name of the reject code (`PROTOCOL_MISMATCH`, `SERVER_FULL`,
    /// `BAD_REQUEST`, `ALREADY_CONNECTED`). Codes are logged verbatim on both
    /// sides and shown to the player as-is, which is why the name is what gets
    /// carried. The one reconnect policy branches on this field through
    /// [`Self::is_character_name_refusal`], never on the detail.
    pub code: &'static str,
    /// Human-readable detail. Never parsed — branch on the code, display this.
    pub detail: String,
}

/// Separates the code from the detail in [`Reject::describe`]'s output, and is what
/// [`Reject::split_description`] looks for. One constant, so the two cannot disagree.
const REJECT_SEPARATOR: &str = ": ";

impl Reject {
    /// Whether another character name is the complete remedy.
    ///
    /// These are the two server-owned name judgements. Both close the connection by
    /// contract, and both leave the admitted account with no character selected, so the
    /// client can reconnect and offer the same creation form again. No other reject code
    /// is widened into a retry: `BAD_REQUEST`, an expired ticket and a full roster all
    /// need a different remedy.
    pub(crate) fn is_character_name_refusal(&self) -> bool {
        matches!(self.code, "CHARACTER_NAME_TAKEN" | "CHARACTER_NAME_REFUSED")
    }

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
    /// The server has made this session inert and begun its removal countdown.
    LeaveStarted(LeaveStarted),
    /// The server accepted or refused the request to stop leaving.
    LeaveCancelResult(LeaveCancelResult),
    /// One accepted chat line. ECS delivery is intentionally a later issue.
    Chat(ChatMessage),
    /// One live party invitation. ECS delivery is intentionally a later issue.
    PartyInvite(PartyInvite),
    /// Complete authoritative corpse contents for this recipient.
    LootState(LootState),
    /// The named corpse container is no longer openable.
    LootClosed(LootClosed),
    /// Complete authoritative learned-mount set for this recipient.
    LearnedMounts(LearnedMounts),
    /// One monster blow that actually reduced this player's authoritative health.
    MobHit(MobHit),
    /// One authoritative square of the map. Decoded and validated here; no ECS system
    /// consumes it until the map window issue, exactly as `MineProgress` was decoded
    /// from V2 and drawn later.
    MapTile(MapTile),
    /// One page of the additive ledger of where this character has been.
    MapExplored(MapExplored),
    /// Every mark this character holds, replacing the client's copy wholesale.
    MarkerList(MarkerList),
    /// What one visible resident is called and what they do. Decoded and validated here;
    /// no ECS system consumes it until the resident issue, exactly as `MineProgress` was
    /// decoded from V2 and drawn later.
    ResidentAppearance(ResidentAppearance),
    /// The complete price list one vendor shows this recipient.
    VendorState(VendorState),
    /// The named stall is no longer open.
    VendorClosed(VendorClosed),
    /// Complete authoritative two-sided state for one open player trade.
    PlayerTradeState(PlayerTradeState),
    /// The open player trade with this partner ended for the stated reason.
    PlayerTradeClosed(PlayerTradeClosed),
    /// Where the blizzard is in its life. Decoded and validated here; no ECS system
    /// consumes it until the storm's countdown (#470), exactly as `MineProgress` was
    /// decoded from V2 and drawn later.
    StormWarning(StormWarning),
    /// Every warded chunk column in view, replacing the client's copy wholesale. Decoded
    /// and validated here; the boundary that draws it is its own issue.
    WardsNearby(WardsNearby),
    /// One Opus frame the server decided this session may hear. Decoded and validated
    /// here; nothing plays it until the client's audio path (#851), exactly as
    /// `MapTile` was carried this far before the map window existed.
    VoiceHeard(VoiceHeard),
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
    /// `equipment_slots` violates `1..=MAX_EQUIPMENT_SLOTS`.
    EquipmentSlots(u8),
    /// The leading hotbar and trailing equipment subsets must fit in the inventory.
    ReservedSlotsExceedInventory {
        hotbar: u8,
        equipment: u8,
        inventory: u8,
    },
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
    /// A snapshot carries a projectile with a NaN or infinite component.
    NonFiniteProjectile {
        entity_id: u64,
        field: &'static str,
        value: f32,
    },
    /// Projectile kind zero, or a member this renderer does not know.
    UnknownProjectileKind { entity_id: u64, value: u8 },
    /// One id names two projectiles in the same complete vector.
    DuplicateProjectile(u64),
    /// Item id 0 is reserved for no item and cannot name a drop.
    DropWithoutItem(u64),
    /// A drop with no items is despawned, never sent.
    EmptyDrop(u64),
    /// One id names two drops in the same snapshot, so sparse state could not name
    /// exactly one of them.
    DuplicateDrop(u64),
    /// Sparse wear names no drop in the fixed drop vector beside it.
    DropDurabilityWithoutDrop(u64),
    /// The sparse wear vector names one drop more than once.
    DropDurabilityNamedTwice(u64),
    /// A durability pair has no maximum, exceeds its maximum, or belongs to a stack.
    DropDurability {
        entity_id: u64,
        count: u16,
        durability: u16,
        max_durability: u16,
    },
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
    /// `max_hunger` is zero, or `hunger` exceeds it. A zero reserve is legal; a zero
    /// denominator is not.
    VitalsHunger { hunger: u16, max_hunger: u16 },
    /// Progression carries no level, no denominator, or more experience than that
    /// denominator. At the level cap equality remains legal.
    VitalsExperience {
        level: u16,
        experience: u32,
        experience_to_next: u32,
    },
    /// An `Alive` player with no health left. Zero health is what the server's own
    /// transition to `Dead` means, so this is a server that has lost track of one of its
    /// own players.
    AliveWithoutHealth,
    /// Only a dead player counts down to a respawn.
    RespawnWhileAlive { respawn_ticks: u32 },
    /// A mob carries the reserved identity 0.
    MobWithoutIdentity,
    /// A hit notification carries the reserved attacker identity 0.
    MobHitWithoutIdentity,
    /// A hit notification carries no attacker position.
    MissingMobHitPosition,
    /// A hit notification carries a NaN or infinite attacker position component.
    NonFiniteMobHit {
        attacker_entity_id: u64,
        axis: usize,
        value: f32,
    },
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
    /// A player state carries the reserved entity id 0.
    EntityWithoutIdentity,
    /// One entity id names two player states in the same snapshot.
    DuplicateEntity(u64),
    /// Sparse mount state names no player in the same snapshot.
    MountNotInSnapshot(u64),
    /// Sparse mount state names one player more than once.
    DuplicateMountState(u64),
    /// Mount state carries the absent zero or a member this build cannot name.
    UnknownMountKind { entity_id: u64, value: u8 },
    /// The recipient's cast carries the absent zero or an unknown kind.
    UnknownCastKind(u8),
    /// A completed cast must leave the snapshot instead of remaining at 255.
    CompletedCast,
    /// `dead_players` names an entity that is not a player in the same snapshot. Refused
    /// rather than remembered: the vector describes the bodies this snapshot carries.
    DeadPlayerNotInSnapshot(u64),
    /// The same entity id appears twice in `dead_players`.
    DeadPlayerNamedTwice(u64),
    /// A blocking id is not visible.
    BlockingPlayerNotInSnapshot(u64),
    /// A blocking id is duplicated.
    BlockingPlayerNamedTwice(u64),
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
    /// A chat line carries the reserved sender id 0.
    ChatWithoutEntity,
    /// A chat line omitted its sender display name. Empty remains legal.
    ChatWithoutName(u64),
    /// A party invitation carries the reserved sender id 0.
    PartyInviteWithoutEntity,
    /// A party invitation omitted its sender display name. Empty remains legal.
    PartyInviteWithoutName(u64),
    /// A party invitation carries no remaining lifetime.
    PartyInviteWithoutTime,
    /// A party member carries the reserved entity id 0.
    PartyMemberWithoutIdentity,
    /// One entity id appears twice in the party projection.
    DuplicatePartyMember(u64),
    /// A party member position contains NaN or infinity.
    NonFinitePartyMember {
        entity_id: u64,
        field: &'static str,
        value: f32,
    },
    /// A party member has no health denominator or exceeds it.
    PartyMemberHealth {
        entity_id: u64,
        health: u16,
        max_health: u16,
    },
    /// Members exist while the leader id says there is no party.
    PartyMembersWithoutLeader,
    /// A stable roster member carries reserved character id zero.
    PartyRosterWithoutCharacter,
    /// A stable character appears twice in the roster.
    DuplicatePartyCharacter(u64),
    /// An online roster member carries no entity, or an offline one carries one.
    PartyRosterOnlineMismatch {
        character_id: u64,
        entity_id: u64,
        online: bool,
    },
    /// A non-zero online entity appears twice in the stable roster.
    DuplicatePartyRosterEntity(u64),
    /// A roster member omitted its display name. Empty remains legal.
    PartyRosterWithoutName(u64),
    /// The legacy live leader projection disagrees with the first stable roster entry.
    PartyLeaderRosterMismatch { expected: u64, actual: u64 },
    /// An online combat member is absent or offline in the stable roster.
    PartyMemberMissingFromRoster(u64),
    /// The accessible-corpse vector carries reserved identity zero.
    AccessibleCorpseWithoutIdentity,
    /// One corpse appears twice in the complete accessibility set.
    DuplicateAccessibleCorpse(u64),
    /// An accessible corpse id does not name a mob in Corpse action in this snapshot.
    AccessibleCorpseWithoutMob(u64),
    /// A loot payload carries reserved corpse identity zero.
    LootWithoutCorpse(&'static str),
    /// A complete state carries no revision.
    LootWithoutRevision,
    /// A complete state omitted its non-empty entry vector.
    LootWithoutEntries(u64),
    /// A loot entry has no stable identity.
    LootEntryWithoutIdentity(u64),
    /// A stable entry id appears twice in one container.
    DuplicateLootEntry { corpse_id: u64, entry_id: u64 },
    /// A loot stack is empty or carries an impossible durability pair.
    InvalidLootEntry {
        corpse_id: u64,
        entry_id: u64,
        item_id: u16,
        count: u16,
        durability: u16,
        max_durability: u16,
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
    /// A V13 description omitted its server-owned display text. Empty is legal;
    /// absence is a pre-V13 message shape and is not.
    AppearanceWithoutName(u64),
    /// A V17 description omitted its current level. The absent scalar reads as zero,
    /// which is not a level any character can have.
    AppearanceWithoutLevel(u64),
    /// `LeaveStarted.remaining_ms` is zero, which describes a countdown already over.
    LeaveWithoutTime,
    /// A cancellation answer disagrees with the shape the contract assigns its outcome.
    LeaveCancelResultShape { accepted: bool, remaining_ms: u32 },
    /// A `MapTile` names a blocks-per-pixel value this contract has no member for. The
    /// absent-field zero is one of them.
    MapTileScale(u8),
    /// A `MapTile` names an origin that is not on the `64 * scale` grid. Checked on this
    /// side as well as the server's, because this is where a tile is placed on a canvas.
    MapTileOffGrid {
        origin_x: i32,
        origin_z: i32,
        scale: u8,
    },
    /// A `MapTile` array is absent or the wrong length. A short array is a protocol
    /// error, never a partially drawn tile.
    MapTileArrayLength {
        field: &'static str,
        len: usize,
        want: usize,
    },
    /// A `MapTile` carries a surface byte this build has no member for.
    UnknownMapSurface { index: usize, value: u8 },
    /// A `MapExplored` page carries no columns. An empty page states nothing, and
    /// reading one as "the ledger is empty" would erase the client's map.
    MapExploredWithoutColumns,
    /// A `MapExplored` page carries more columns than the contract's bound.
    MapExploredTooManyColumns(usize),
    /// A `MarkerList` carries more marks than one character may hold.
    TooManyMarkers(usize),
    /// A `Marker` carries the reserved id 0.
    MarkerWithoutIdentity,
    /// One `marker_id` names two marks of the same list.
    DuplicateMarker(u64),
    /// A `Marker` carries a kind this build has no member for, `Unknown` included.
    UnknownMarkerKind { marker_id: u64, value: u8 },
    /// A `Marker` note is longer than the contract allows, measured in bytes.
    MarkerNoteTooLong { marker_id: u64, len: usize },
    /// A `ResidentAppearance` carries the reserved entity id 0.
    ResidentWithoutEntity,
    /// A `ResidentAppearance` omitted its server-owned name. Absence is refused; so is
    /// empty, because a resident the server placed always has one.
    ResidentWithoutName(u64),
    /// A resident's name is longer than the contract allows, measured in bytes.
    ResidentNameTooLong { entity_id: u64, len: usize },
    /// A `ResidentAppearance` carries a role this build has no member for, the
    /// absent-field `Unknown` included.
    UnknownResidentRole { entity_id: u64, value: u8 },
    /// LearnedMounts carries the absent zero or an unknown mount.
    UnknownLearnedMount(u8),
    /// LearnedMounts names one mount more than once.
    DuplicateLearnedMount(MountKind),
    /// A `VendorState` or `VendorClosed` carries the reserved entity id 0.
    VendorWithoutEntity(&'static str),
    /// A `VendorState` carries revision 0, which names no list.
    VendorWithoutRevision,
    /// A `VendorState` omitted one of its two price vectors. Empty is legal — a vendor
    /// that only buys, or only sells — and absent is a message shape this contract does
    /// not have.
    VendorWithoutPrices { entity_id: u64, field: &'static str },
    /// Both of a `VendorState`'s vectors are empty. A vendor with nothing to say is
    /// `VendorClosed`, not a stall that opens onto nothing.
    VendorWithNothingToTrade(u64),
    /// A `VendorEntry` names item 0, which the registry never mints.
    VendorEntryWithoutItem { entity_id: u64, field: &'static str },
    /// A `VendorEntry` carries price 0. Free is not a price, in either direction.
    VendorEntryWithoutPrice {
        entity_id: u64,
        field: &'static str,
        item_id: u16,
    },
    /// One item id appears twice in one `VendorState` vector, which is two prices for one
    /// thing and no way to tell which a `TradeRequest` meant.
    DuplicateVendorEntry {
        entity_id: u64,
        field: &'static str,
        item_id: u16,
    },
    /// A player-trade payload carries partner id 0.
    PlayerTradeWithoutPartner(&'static str),
    /// Partner text is absent; empty remains legal.
    PlayerTradeWithoutPartnerName,
    /// Revision 0 names no state.
    PlayerTradeWithoutRevision,
    /// A required complete offer vector is absent.
    PlayerTradeWithoutOffer(&'static str),
    /// An offer exceeds the contract's five positions.
    PlayerTradeOfferTooLarge { field: &'static str, len: usize },
    /// An offer index is outside `0..PLAYER_TRADE_SLOTS`.
    PlayerTradeSlotOutOfRange {
        field: &'static str,
        index: usize,
        trade_slot: u8,
    },
    /// One offer repeats a trade position.
    DuplicatePlayerTradeSlot { field: &'static str, trade_slot: u8 },
    /// An offered stack has count 0.
    EmptyPlayerTradeSlot { field: &'static str, index: usize },
    /// Durability is present without a maximum.
    PlayerTradeDurabilityWithoutMaximum {
        field: &'static str,
        index: usize,
        durability: u16,
    },
    /// Durability exceeds its maximum.
    PlayerTradeDurabilityExceedsMaximum {
        field: &'static str,
        index: usize,
        durability: u16,
        max_durability: u16,
    },
    /// A durable stack has a count other than one.
    PlayerTradeDurableStackCount {
        field: &'static str,
        index: usize,
        count: u16,
    },
    /// The partner's private pack position was exposed.
    PlayerTradePartnerPackSlot { index: usize, pack_slot: u8 },
    /// A present `WeatherState` names a kind this build has no member for, the
    /// absent-field `Unknown` included.
    ///
    /// Absence of the whole struct is the legal "this server keeps no weather" and never
    /// reaches here: it is `Snapshot::weather == None`. This is a struct that arrived and
    /// then said nothing.
    UnknownWeatherKind { value: u8 },
    /// A `WeatherState` says `Clear` and carries a non-zero intensity.
    ///
    /// Refused rather than clamped to either half, for the reason a broken world clock is
    /// refused rather than repaired: the two fields are describing different skies and
    /// nothing here can tell which one the server is simulating.
    ClearWeatherWithIntensity { intensity: u8 },
    /// A `StormWarning` carries a phase this build has no member for, the absent-field
    /// `Unknown` included. There is no phase to read `seconds_until` against.
    UnknownStormPhase { value: u8 },
    /// A `StormWarning` says the storm has passed and still carries a countdown. The two
    /// statements are about different storms.
    StormPassedWithCountdown { seconds_until: u32 },
    /// A `WardsNearby` carries more columns than the contract's bound.
    ///
    /// Refused rather than truncated: a receiver that dropped the tail would shade the
    /// world wrong and say nothing about it.
    TooManyWardedColumns(usize),
    /// A `WardedColumn` carries a ward kind this build has no member for, the
    /// absent-field `Unknown` included.
    UnknownWardKind { cx: i32, cz: i32, value: u8 },
    /// One `(cx, cz)` appears twice in the same `WardsNearby`. Two rows for one column
    /// are two answers about the same ground with no way to tell which is meant.
    DuplicateWardedColumn { cx: i32, cz: i32 },
    /// `ServerWelcome.voice_range_blocks` is negative or non-finite.
    ///
    /// **Zero is deliberately not in this refusal**: it is the legal announcement of a
    /// server that relays no voice, not a degenerate radius. Refused rather than
    /// clamped, for the reason a spawn axis is — NaN compares false against every
    /// bound, so a clamp would pass it through untouched.
    VoiceRange(f32),
    /// A `VoiceHeard` carries the reserved entity id 0, so nothing names the speaker
    /// and no snapshot can place the voice.
    VoiceWithoutSpeaker,
    /// A `VoiceHeard` carries no audio: an absent or empty `opus` vector. A frame with
    /// nothing in it is one the server should not have relayed.
    VoiceWithoutAudio { speaker_entity_id: u64 },
    /// A `VoiceHeard` carries more Opus bytes than `VoiceBound.MaxOpusBytes`.
    ///
    /// Refused rather than truncated: half a frame is not a frame, and this side
    /// allocates from a length the peer chose. The length is named and the bytes are
    /// not, which is the rule for every diagnostic that touches this payload.
    OversizedVoiceFrame { speaker_entity_id: u64, len: usize },
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
            Self::EquipmentSlots(got) => {
                write!(
                    f,
                    "equipment slot count must be in 1..={MAX_EQUIPMENT_SLOTS}, got {got}"
                )
            }
            Self::ReservedSlotsExceedInventory {
                hotbar,
                equipment,
                inventory,
            } => write!(
                f,
                "hotbar has {hotbar} slots and equipment has {equipment}, more than the inventory's {inventory}"
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
            Self::NonFiniteProjectile {
                entity_id,
                field,
                value,
            } => write!(
                f,
                "projectile {entity_id} has a non-finite {field}: {value}"
            ),
            Self::UnknownProjectileKind { entity_id, value } => {
                write!(f, "projectile {entity_id} carries unknown kind {value}")
            }
            Self::DuplicateProjectile(entity_id) => write!(
                f,
                "projectile entity id {entity_id} appears twice in one snapshot"
            ),
            Self::DropWithoutItem(entity_id) => {
                write!(f, "drop {entity_id} carries reserved item id 0")
            }
            Self::EmptyDrop(entity_id) => write!(f, "drop {entity_id} has count zero"),
            Self::DuplicateDrop(entity_id) => {
                write!(
                    f,
                    "drop entity id {entity_id} appears twice in one snapshot"
                )
            }
            Self::DropDurabilityWithoutDrop(entity_id) => write!(
                f,
                "drop_durabilities names {entity_id}, which is not a drop in this snapshot"
            ),
            Self::DropDurabilityNamedTwice(entity_id) => {
                write!(f, "drop_durabilities names {entity_id} twice")
            }
            Self::DropDurability {
                entity_id,
                count,
                durability,
                max_durability,
            } => write!(
                f,
                "drop {entity_id} has count {count} and durability {durability}/{max_durability}, which is not a possible durable drop"
            ),
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
            Self::VitalsHunger { hunger, max_hunger } => write!(
                f,
                "hunger is {hunger}/{max_hunger}, want a non-zero maximum and no more hunger than it"
            ),
            Self::VitalsExperience {
                level,
                experience,
                experience_to_next,
            } => write!(
                f,
                "progression is level {level} at {experience}/{experience_to_next}, want a non-zero level and denominator and no more experience than it"
            ),
            Self::AliveWithoutHealth => write!(f, "vitals say alive with no health left"),
            Self::RespawnWhileAlive { respawn_ticks } => write!(
                f,
                "vitals count {respawn_ticks} ticks to a respawn for a player who is not dead"
            ),
            Self::MobWithoutIdentity => write!(f, "a mob carries the reserved entity id 0"),
            Self::MobHitWithoutIdentity => {
                write!(f, "a mob hit carries the reserved attacker entity id 0")
            }
            Self::MissingMobHitPosition => write!(f, "a mob hit carries no attacker position"),
            Self::NonFiniteMobHit {
                attacker_entity_id,
                axis,
                value,
            } => write!(
                f,
                "mob hit from {attacker_entity_id} has a non-finite position component {axis}: {value}"
            ),
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
            Self::EntityWithoutIdentity => {
                write!(f, "a player state carries the reserved entity id 0")
            }
            Self::DuplicateEntity(entity_id) => {
                write!(f, "entity id {entity_id} names two players in one snapshot")
            }
            Self::MountNotInSnapshot(entity_id) => write!(
                f,
                "mounts names {entity_id}, which is not a player in this snapshot"
            ),
            Self::DuplicateMountState(entity_id) => {
                write!(f, "mounts names player {entity_id} twice")
            }
            Self::UnknownMountKind { entity_id, value } => {
                write!(
                    f,
                    "mount state for player {entity_id} has unknown mount {value}"
                )
            }
            Self::UnknownCastKind(value) => {
                write!(f, "self_cast has unknown kind {value}")
            }
            Self::CompletedCast => write!(f, "self_cast remains present at completed progress 255"),
            Self::DeadPlayerNotInSnapshot(entity_id) => write!(
                f,
                "dead_players names {entity_id}, which is not a player in this snapshot"
            ),
            Self::DeadPlayerNamedTwice(entity_id) => {
                write!(f, "dead_players names {entity_id} twice")
            }
            Self::BlockingPlayerNotInSnapshot(entity_id) => write!(
                f,
                "blocking_players names {entity_id}, which is not a player in this snapshot"
            ),
            Self::BlockingPlayerNamedTwice(entity_id) => {
                write!(f, "blocking_players names {entity_id} twice")
            }
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
            Self::ChatWithoutEntity => write!(f, "a ChatMessage carries reserved entity id 0"),
            Self::ChatWithoutName(entity_id) => {
                write!(
                    f,
                    "ChatMessage for entity {entity_id} carries no sender name"
                )
            }
            Self::PartyInviteWithoutEntity => {
                write!(f, "a PartyInvite carries reserved entity id 0")
            }
            Self::PartyInviteWithoutName(entity_id) => {
                write!(
                    f,
                    "PartyInvite from entity {entity_id} carries no sender name"
                )
            }
            Self::PartyInviteWithoutTime => {
                write!(f, "PartyInvite carries no time remaining")
            }
            Self::PartyMemberWithoutIdentity => {
                write!(f, "a party member carries reserved entity id 0")
            }
            Self::DuplicatePartyMember(entity_id) => {
                write!(f, "party_members names {entity_id} twice")
            }
            Self::NonFinitePartyMember {
                entity_id,
                field,
                value,
            } => write!(
                f,
                "party member {entity_id} has a non-finite {field}: {value}"
            ),
            Self::PartyMemberHealth {
                entity_id,
                health,
                max_health,
            } => write!(
                f,
                "party member {entity_id} is {health}/{max_health}, want a non-zero maximum and no more health than it"
            ),
            Self::PartyMembersWithoutLeader => {
                write!(f, "party_members is non-empty while party leader id is 0")
            }
            Self::PartyRosterWithoutCharacter => {
                write!(f, "a party roster member carries reserved character id 0")
            }
            Self::DuplicatePartyCharacter(character_id) => {
                write!(f, "party_roster names character {character_id} twice")
            }
            Self::PartyRosterOnlineMismatch {
                character_id,
                entity_id,
                online,
            } => write!(
                f,
                "party roster character {character_id} has entity {entity_id} with online={online}"
            ),
            Self::DuplicatePartyRosterEntity(entity_id) => {
                write!(f, "party_roster names online entity {entity_id} twice")
            }
            Self::PartyRosterWithoutName(character_id) => {
                write!(f, "party roster character {character_id} carries no name")
            }
            Self::PartyLeaderRosterMismatch { expected, actual } => write!(
                f,
                "party leader entity id is {actual}, want roster leader entity id {expected}"
            ),
            Self::PartyMemberMissingFromRoster(entity_id) => {
                write!(
                    f,
                    "party member entity {entity_id} is not online in party_roster"
                )
            }
            Self::AccessibleCorpseWithoutIdentity => {
                write!(f, "accessible_loot_corpses carries reserved id 0")
            }
            Self::DuplicateAccessibleCorpse(corpse_id) => {
                write!(f, "accessible_loot_corpses names {corpse_id} twice")
            }
            Self::AccessibleCorpseWithoutMob(corpse_id) => write!(
                f,
                "accessible loot corpse {corpse_id} does not name a MobAction::Corpse"
            ),
            Self::LootWithoutCorpse(kind) => write!(f, "{kind} carries reserved corpse id 0"),
            Self::LootWithoutRevision => write!(f, "LootState carries revision 0"),
            Self::LootWithoutEntries(corpse_id) => {
                write!(f, "LootState for corpse {corpse_id} carries no entries")
            }
            Self::LootEntryWithoutIdentity(corpse_id) => {
                write!(f, "LootState for corpse {corpse_id} carries entry id 0")
            }
            Self::DuplicateLootEntry {
                corpse_id,
                entry_id,
            } => {
                write!(
                    f,
                    "LootState for corpse {corpse_id} names entry {entry_id} twice"
                )
            }
            Self::InvalidLootEntry {
                corpse_id,
                entry_id,
                item_id,
                count,
                durability,
                max_durability,
            } => write!(
                f,
                "LootState corpse {corpse_id} entry {entry_id} is item {item_id} x{count} at {durability}/{max_durability}"
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
            Self::AppearanceWithoutName(entity_id) => {
                write!(f, "PlayerAppearance for entity {entity_id} carries no name")
            }
            Self::AppearanceWithoutLevel(entity_id) => {
                write!(
                    f,
                    "PlayerAppearance for entity {entity_id} carries no level"
                )
            }
            Self::LeaveWithoutTime => write!(f, "LeaveStarted carries no time remaining"),
            Self::LeaveCancelResultShape {
                accepted,
                remaining_ms,
            } => write!(
                f,
                "LeaveCancelResult accepted={accepted} carries remaining_ms={remaining_ms}"
            ),
            Self::MapTileScale(value) => write!(f, "map scale {value} is not 1, 4 or 16"),
            Self::MapTileOffGrid {
                origin_x,
                origin_z,
                scale,
            } => write!(
                f,
                "map tile origin ({origin_x}, {origin_z}) is not on the grid scale {scale} sets"
            ),
            Self::MapTileArrayLength { field, len, want } => {
                write!(f, "MapTile.{field} has {len} entries, want {want}")
            }
            Self::UnknownMapSurface { index, value } => {
                write!(f, "MapTile.surface[{index}] is an unknown surface: {value}")
            }
            Self::MapExploredWithoutColumns => {
                write!(f, "a MapExplored page carries no columns")
            }
            Self::MapExploredTooManyColumns(len) => write!(
                f,
                "a MapExplored page carries {len} columns, at most {MAX_EXPLORED_COLUMNS}"
            ),
            Self::TooManyMarkers(len) => {
                write!(f, "a MarkerList carries {len} marks, at most {MAX_MARKERS}")
            }
            Self::MarkerWithoutIdentity => write!(f, "a Marker carries the reserved id 0"),
            Self::DuplicateMarker(marker_id) => {
                write!(f, "a MarkerList names mark {marker_id} twice")
            }
            Self::UnknownMarkerKind { marker_id, value } => {
                write!(f, "mark {marker_id} has an unknown kind: {value}")
            }
            Self::MarkerNoteTooLong { marker_id, len } => write!(
                f,
                "mark {marker_id} has a {len}-byte note, at most {MARKER_NOTE_MAX_BYTES}"
            ),
            Self::ResidentWithoutEntity => {
                write!(f, "a ResidentAppearance carries the reserved entity id 0")
            }
            Self::ResidentWithoutName(entity_id) => {
                write!(f, "ResidentAppearance for entity {entity_id} has no name")
            }
            Self::ResidentNameTooLong { entity_id, len } => write!(
                f,
                "resident {entity_id} has a {len}-byte name, at most {RESIDENT_NAME_MAX_BYTES}"
            ),
            Self::UnknownResidentRole { entity_id, value } => {
                write!(f, "resident {entity_id} has an unknown role: {value}")
            }
            Self::UnknownLearnedMount(value) => {
                write!(f, "LearnedMounts carries unknown mount {value}")
            }
            Self::DuplicateLearnedMount(mount) => {
                write!(f, "LearnedMounts names {mount:?} twice")
            }
            Self::VendorWithoutEntity(message) => {
                write!(f, "{message} carries the reserved entity id 0")
            }
            Self::VendorWithoutRevision => write!(f, "a VendorState carries revision 0"),
            Self::VendorWithoutPrices { entity_id, field } => {
                write!(f, "VendorState for vendor {entity_id} has no {field}")
            }
            Self::VendorWithNothingToTrade(entity_id) => write!(
                f,
                "VendorState for vendor {entity_id} neither buys nor sells anything"
            ),
            Self::VendorEntryWithoutItem { entity_id, field } => {
                write!(f, "vendor {entity_id} {field} names item 0")
            }
            Self::VendorEntryWithoutPrice {
                entity_id,
                field,
                item_id,
            } => write!(f, "vendor {entity_id} {field} prices item {item_id} at 0"),
            Self::DuplicateVendorEntry {
                entity_id,
                field,
                item_id,
            } => write!(f, "vendor {entity_id} {field} names item {item_id} twice"),
            Self::PlayerTradeWithoutPartner(message) => {
                write!(f, "{message} carries the reserved partner entity id 0")
            }
            Self::PlayerTradeWithoutPartnerName => {
                write!(f, "a PlayerTradeState carries no partner name")
            }
            Self::PlayerTradeWithoutRevision => {
                write!(f, "a PlayerTradeState carries revision 0")
            }
            Self::PlayerTradeWithoutOffer(field) => {
                write!(f, "a PlayerTradeState carries no {field}")
            }
            Self::PlayerTradeOfferTooLarge { field, len } => write!(
                f,
                "a PlayerTradeState {field} carries {len} entries, at most {PLAYER_TRADE_SLOTS}"
            ),
            Self::PlayerTradeSlotOutOfRange {
                field,
                index,
                trade_slot,
            } => write!(
                f,
                "PlayerTradeState {field} entry {index} names trade slot {trade_slot}, outside {PLAYER_TRADE_SLOTS} slots"
            ),
            Self::DuplicatePlayerTradeSlot { field, trade_slot } => write!(
                f,
                "PlayerTradeState {field} names trade slot {trade_slot} twice"
            ),
            Self::EmptyPlayerTradeSlot { field, index } => {
                write!(f, "PlayerTradeState {field} entry {index} carries count 0")
            }
            Self::PlayerTradeDurabilityWithoutMaximum {
                field,
                index,
                durability,
            } => write!(
                f,
                "PlayerTradeState {field} entry {index} carries durability {durability} without a maximum"
            ),
            Self::PlayerTradeDurabilityExceedsMaximum {
                field,
                index,
                durability,
                max_durability,
            } => write!(
                f,
                "PlayerTradeState {field} entry {index} durability {durability} exceeds maximum {max_durability}"
            ),
            Self::PlayerTradeDurableStackCount {
                field,
                index,
                count,
            } => write!(
                f,
                "PlayerTradeState {field} entry {index} carries durable count {count}, want 1"
            ),
            Self::PlayerTradePartnerPackSlot { index, pack_slot } => write!(
                f,
                "PlayerTradeState their_offer entry {index} exposes pack slot {pack_slot}"
            ),
            Self::UnknownWeatherKind { value } => {
                write!(f, "a snapshot carries an unknown weather kind: {value}")
            }
            Self::ClearWeatherWithIntensity { intensity } => write!(
                f,
                "clear weather carries intensity {intensity}, and clear is always 0"
            ),
            Self::UnknownStormPhase { value } => {
                write!(f, "a StormWarning carries an unknown phase: {value}")
            }
            Self::StormPassedWithCountdown { seconds_until } => write!(
                f,
                "a passed storm carries {seconds_until} seconds, and a passed storm carries 0"
            ),
            Self::TooManyWardedColumns(len) => write!(
                f,
                "a WardsNearby carries {len} columns, at most {MAX_WARDED_COLUMNS}"
            ),
            Self::UnknownWardKind { cx, cz, value } => write!(
                f,
                "warded column ({cx}, {cz}) has an unknown ward kind: {value}"
            ),
            Self::DuplicateWardedColumn { cx, cz } => {
                write!(f, "a WardsNearby names column ({cx}, {cz}) twice")
            }
            Self::VoiceRange(got) => {
                write!(f, "voice range must be finite and not negative, got {got}")
            }
            Self::VoiceWithoutSpeaker => {
                write!(f, "a VoiceHeard carries reserved entity id 0")
            }
            Self::VoiceWithoutAudio { speaker_entity_id } => write!(
                f,
                "a VoiceHeard from entity {speaker_entity_id} carries no audio"
            ),
            Self::OversizedVoiceFrame {
                speaker_entity_id,
                len,
            } => write!(
                f,
                "a VoiceHeard from entity {speaker_entity_id} carries {len} Opus bytes, \
                 at most {MAX_OPUS_BYTES}"
            ),
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
            let name = payload
                .name()
                .ok_or(DecodeError::AppearanceWithoutName(entity_id))?
                .to_owned();
            let level = payload.level();
            if level == 0 {
                return Err(DecodeError::AppearanceWithoutLevel(entity_id));
            }
            // Nothing here asks whether the client knows this entity, and nothing may:
            // `schemas/player.fbs` is explicit that the appearance stream and the
            // snapshot stream are not ordered against each other, so an appearance for
            // an entity nobody has seen yet is the ordinary case rather than an error.
            Ok(Message::PlayerAppearance(PlayerAppearance {
                entity_id,
                appearance: appearance(payload.appearance(), "PlayerAppearance")?,
                name,
                level,
                worn_head: payload.worn_head(),
                worn_chest: payload.worn_chest(),
                worn_legs: payload.worn_legs(),
                worn_offhand: payload.worn_offhand(),
            }))
        }
        fb::Payload::LeaveStarted => {
            let payload = envelope
                .payload_as_leave_started()
                .ok_or(DecodeError::MissingPayload(name))?;
            let remaining_ms = payload.remaining_ms();
            if remaining_ms == 0 {
                return Err(DecodeError::LeaveWithoutTime);
            }
            Ok(Message::LeaveStarted(LeaveStarted { remaining_ms }))
        }
        fb::Payload::LeaveCancelResult => {
            let payload = envelope
                .payload_as_leave_cancel_result()
                .ok_or(DecodeError::MissingPayload(name))?;
            let accepted = payload.accepted();
            let remaining_ms = payload.remaining_ms();
            if accepted == (remaining_ms != 0) {
                return Err(DecodeError::LeaveCancelResultShape {
                    accepted,
                    remaining_ms,
                });
            }
            Ok(Message::LeaveCancelResult(LeaveCancelResult {
                accepted,
                remaining_ms,
            }))
        }
        fb::Payload::ChatMessage => {
            let payload = envelope
                .payload_as_chat_message()
                .ok_or(DecodeError::MissingPayload(name))?;
            let sender_entity_id = payload.sender_entity_id();
            if sender_entity_id == 0 {
                return Err(DecodeError::ChatWithoutEntity);
            }
            let sender_name = payload
                .sender_name()
                .ok_or(DecodeError::ChatWithoutName(sender_entity_id))?
                .to_owned();
            Ok(Message::Chat(ChatMessage {
                sender_entity_id,
                sender_name,
                // Display text copied verbatim. Absent and empty carry the same line.
                text: payload.text().unwrap_or_default().to_owned(),
            }))
        }
        fb::Payload::PartyInvite => {
            let payload = envelope
                .payload_as_party_invite()
                .ok_or(DecodeError::MissingPayload(name))?;
            let from_entity_id = payload.from_entity_id();
            if from_entity_id == 0 {
                return Err(DecodeError::PartyInviteWithoutEntity);
            }
            let from_name = payload
                .from_name()
                .ok_or(DecodeError::PartyInviteWithoutName(from_entity_id))?
                .to_owned();
            let expires_ms = payload.expires_ms();
            if expires_ms == 0 {
                return Err(DecodeError::PartyInviteWithoutTime);
            }
            Ok(Message::PartyInvite(PartyInvite {
                from_entity_id,
                from_name,
                expires_ms,
            }))
        }
        fb::Payload::LootState => {
            let payload = envelope
                .payload_as_loot_state()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::LootState(loot_state(&payload)?))
        }
        fb::Payload::LootClosed => {
            let payload = envelope
                .payload_as_loot_closed()
                .ok_or(DecodeError::MissingPayload(name))?;
            let corpse_id = payload.corpse_id();
            if corpse_id == 0 {
                return Err(DecodeError::LootWithoutCorpse("LootClosed"));
            }
            Ok(Message::LootClosed(LootClosed { corpse_id }))
        }
        fb::Payload::LearnedMounts => {
            let payload = envelope
                .payload_as_learned_mounts()
                .ok_or(DecodeError::MissingPayload(name))?;
            let mut mounts = Vec::new();
            let mut seen = HashSet::new();
            if let Some(list) = payload.mounts() {
                mounts.reserve(list.len());
                for value in list.iter() {
                    let mount = MountKind::from_wire(value)
                        .ok_or(DecodeError::UnknownLearnedMount(value.0))?;
                    if !seen.insert(mount) {
                        return Err(DecodeError::DuplicateLearnedMount(mount));
                    }
                    mounts.push(mount);
                }
            }
            Ok(Message::LearnedMounts(LearnedMounts { mounts }))
        }
        fb::Payload::MobHit => {
            let payload = envelope
                .payload_as_mob_hit()
                .ok_or(DecodeError::MissingPayload(name))?;
            let attacker_entity_id = payload.attacker_entity_id();
            if attacker_entity_id == 0 {
                return Err(DecodeError::MobHitWithoutIdentity);
            }
            let pos = payload
                .attacker_pos()
                .ok_or(DecodeError::MissingMobHitPosition)?;
            let attacker_pos = [pos.x(), pos.y(), pos.z()];
            for (axis, value) in attacker_pos.into_iter().enumerate() {
                if !value.is_finite() {
                    return Err(DecodeError::NonFiniteMobHit {
                        attacker_entity_id,
                        axis,
                        value,
                    });
                }
            }
            Ok(Message::MobHit(MobHit {
                attacker_entity_id,
                attacker_pos,
            }))
        }
        fb::Payload::MapTile => {
            let payload = envelope
                .payload_as_map_tile()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::MapTile(map_tile(&payload)?))
        }
        fb::Payload::MapExplored => {
            let payload = envelope
                .payload_as_map_explored()
                .ok_or(DecodeError::MissingPayload(name))?;
            let columns = payload
                .columns()
                .ok_or(DecodeError::MapExploredWithoutColumns)?;
            if columns.is_empty() {
                return Err(DecodeError::MapExploredWithoutColumns);
            }
            if columns.len() > MAX_EXPLORED_COLUMNS {
                return Err(DecodeError::MapExploredTooManyColumns(columns.len()));
            }
            // Duplicates are deliberately not refused: the ledger is additive, so a
            // column named twice has told this client the same true thing twice. That is
            // the difference from `MarkerList`, where two rows with one id are two
            // different marks and the receiver cannot tell which is meant.
            Ok(Message::MapExplored(MapExplored {
                columns: columns
                    .iter()
                    .map(|column| MapColumn {
                        cx: column.cx(),
                        cz: column.cz(),
                    })
                    .collect(),
            }))
        }
        fb::Payload::MarkerList => {
            let payload = envelope
                .payload_as_marker_list()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::MarkerList(marker_list(&payload)?))
        }
        fb::Payload::ResidentAppearance => {
            let payload = envelope
                .payload_as_resident_appearance()
                .ok_or(DecodeError::MissingPayload(name))?;
            let entity_id = payload.entity_id();
            if entity_id == 0 {
                return Err(DecodeError::ResidentWithoutEntity);
            }
            // Absent and empty are one refusal here, which is the opposite of the call
            // `PlayerAppearance` makes: a player's stored name may legitimately be empty
            // and the server copies it verbatim, whereas a resident is named by the world
            // generator and an unnamed one is a defect rather than a choice.
            let resident_name = payload
                .name()
                .filter(|text| !text.is_empty())
                .ok_or(DecodeError::ResidentWithoutName(entity_id))?;
            if resident_name.len() > RESIDENT_NAME_MAX_BYTES {
                return Err(DecodeError::ResidentNameTooLong {
                    entity_id,
                    len: resident_name.len(),
                });
            }
            let role = ResidentRole::from_wire(payload.role()).ok_or(
                DecodeError::UnknownResidentRole {
                    entity_id,
                    value: payload.role().0,
                },
            )?;
            // Nothing here asks whether the client knows this entity, and nothing may:
            // the appearance stream and the snapshot stream are not ordered against each
            // other, exactly as `PlayerAppearance` records.
            Ok(Message::ResidentAppearance(ResidentAppearance {
                entity_id,
                name: resident_name.to_owned(),
                role,
                appearance: appearance(payload.appearance(), "ResidentAppearance")?,
            }))
        }
        fb::Payload::VendorState => {
            let payload = envelope
                .payload_as_vendor_state()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::VendorState(vendor_state(&payload)?))
        }
        fb::Payload::VendorClosed => {
            let payload = envelope
                .payload_as_vendor_closed()
                .ok_or(DecodeError::MissingPayload(name))?;
            let entity_id = payload.entity_id();
            if entity_id == 0 {
                return Err(DecodeError::VendorWithoutEntity("VendorClosed"));
            }
            Ok(Message::VendorClosed(VendorClosed { entity_id }))
        }
        fb::Payload::PlayerTradeState => {
            let payload = envelope
                .payload_as_player_trade_state()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::PlayerTradeState(player_trade_state(&payload)?))
        }
        fb::Payload::PlayerTradeClosed => {
            let payload = envelope
                .payload_as_player_trade_closed()
                .ok_or(DecodeError::MissingPayload(name))?;
            let partner_entity_id = payload.partner_entity_id();
            if partner_entity_id == 0 {
                return Err(DecodeError::PlayerTradeWithoutPartner("PlayerTradeClosed"));
            }
            Ok(Message::PlayerTradeClosed(PlayerTradeClosed {
                partner_entity_id,
                reason: PlayerTradeCloseReason::from_wire(payload.reason()),
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
        | fb::Payload::CreateCharacterRequest
        | fb::Payload::DropItemRequest
        | fb::Payload::LeaveRequest
        | fb::Payload::LeaveCancelRequest
        | fb::Payload::ConsumeRequest
        | fb::Payload::ChatRequest
        | fb::Payload::PartyRequest
        | fb::Payload::LootOpenRequest
        | fb::Payload::LootTakeRequest
        | fb::Payload::LootTakeAllRequest
        | fb::Payload::MapTileRequest
        | fb::Payload::MarkerPlaceRequest
        | fb::Payload::MarkerRemoveRequest
        | fb::Payload::NpcInteractRequest
        | fb::Payload::TradeRequest
        | fb::Payload::MountRequest
        | fb::Payload::DismountRequest
        | fb::Payload::PlayerTradeRequest
        | fb::Payload::VoiceFrame
        | fb::Payload::BlockRequest => Ok(Message::ClientOnly(name)),
        // V26's two server→client payloads. Both are read and validated here and neither
        // is drawn yet: the precipitation volume is #466, the storm's countdown is #470
        // and the ward boundary is its own issue. Validating at the decode boundary is
        // the point of carrying them this far — a phase the client cannot name, or a
        // ward list longer than the contract allows, ends the session now rather than
        // when somebody writes the renderer.
        fb::Payload::StormWarning => {
            let payload = envelope
                .payload_as_storm_warning()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::StormWarning(storm_warning(&payload)?))
        }
        fb::Payload::WardsNearby => {
            let payload = envelope
                .payload_as_wards_nearby()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::WardsNearby(wards_nearby(&payload)?))
        }
        // V30's relayed voice. Read and validated here and played nowhere yet — the
        // audio path is #851 — which is the point of carrying it this far: a frame that
        // names no speaker, or one longer than the contract's ceiling, ends the session
        // now rather than when somebody writes the decoder that would allocate from it.
        fb::Payload::VoiceHeard => {
            let payload = envelope
                .payload_as_voice_heard()
                .ok_or(DecodeError::MissingPayload(name))?;
            Ok(Message::VoiceHeard(voice_heard(&payload)?))
        }
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

/// Copies and validates one square of the map.
///
/// The scale is read first because everything else is measured against it: the origin
/// grid, and the length of the explored mask. A scale this contract has no member for
/// leaves nothing to check the rest against, which is why it is the first refusal.
fn map_tile(tile: &fb::MapTile<'_>) -> Result<MapTile, DecodeError> {
    let scale = tile.scale();
    let span = map_tile_span(scale).ok_or(DecodeError::MapTileScale(scale))?;
    let (origin_x, origin_z) = (tile.origin_x(), tile.origin_z());
    if origin_x % span != 0 || origin_z % span != 0 {
        return Err(DecodeError::MapTileOffGrid {
            origin_x,
            origin_z,
            scale,
        });
    }

    let height = tile.height().ok_or(DecodeError::MapTileArrayLength {
        field: "height",
        len: 0,
        want: MAP_TILE_CELLS,
    })?;
    if height.len() != MAP_TILE_CELLS {
        return Err(DecodeError::MapTileArrayLength {
            field: "height",
            len: height.len(),
            want: MAP_TILE_CELLS,
        });
    }
    let surface = tile.surface().ok_or(DecodeError::MapTileArrayLength {
        field: "surface",
        len: 0,
        want: MAP_TILE_CELLS,
    })?;
    if surface.len() != MAP_TILE_CELLS {
        return Err(DecodeError::MapTileArrayLength {
            field: "surface",
            len: surface.len(),
            want: MAP_TILE_CELLS,
        });
    }
    // Unwrapped rather than answered: the scale was checked above, and the two functions
    // agree on the same three members by construction.
    let want_explored = map_tile_explored_bytes(scale).ok_or(DecodeError::MapTileScale(scale))?;
    let explored = tile.explored().ok_or(DecodeError::MapTileArrayLength {
        field: "explored",
        len: 0,
        want: want_explored,
    })?;
    if explored.len() != want_explored {
        return Err(DecodeError::MapTileArrayLength {
            field: "explored",
            len: explored.len(),
            want: want_explored,
        });
    }

    let mut surfaces = Vec::with_capacity(MAP_TILE_CELLS);
    for (index, value) in surface.iter().enumerate() {
        let named = MapSurface::from_wire(fb::MapSurface(value))
            .ok_or(DecodeError::UnknownMapSurface { index, value })?;
        surfaces.push(named);
    }

    Ok(MapTile {
        origin_x,
        origin_z,
        scale,
        height: height.iter().collect(),
        surface: surfaces,
        explored: explored.iter().collect(),
    })
}

/// Copies and validates the complete list of marks one character holds.
///
/// An empty list is accepted, and it is the one a character who has marked nothing
/// receives. Ids are checked for uniqueness because this list *replaces* the client's
/// copy: two rows sharing an id are two different marks with one address, and nothing
/// downstream could tell which of them a removal names.
fn marker_list(list: &fb::MarkerList<'_>) -> Result<MarkerList, DecodeError> {
    let markers = list.markers().unwrap_or_default();
    if markers.len() > MAX_MARKERS {
        return Err(DecodeError::TooManyMarkers(markers.len()));
    }

    let mut decoded = Vec::with_capacity(markers.len());
    let mut identities = HashSet::new();
    for marker in &markers {
        let marker_id = marker.marker_id();
        if marker_id == 0 {
            return Err(DecodeError::MarkerWithoutIdentity);
        }
        if !identities.insert(marker_id) {
            return Err(DecodeError::DuplicateMarker(marker_id));
        }
        let kind = MarkerKind::from_wire(marker.kind()).ok_or(DecodeError::UnknownMarkerKind {
            marker_id,
            value: marker.kind().0,
        })?;
        // Absent and empty are the same empty note. The bound is bytes, which is what
        // `str::len` counts.
        let note = marker.note().unwrap_or_default();
        if note.len() > MARKER_NOTE_MAX_BYTES {
            return Err(DecodeError::MarkerNoteTooLong {
                marker_id,
                len: note.len(),
            });
        }
        decoded.push(Marker {
            marker_id,
            x: marker.x(),
            z: marker.z(),
            kind,
            note: note.to_owned(),
        });
    }
    Ok(MarkerList { markers: decoded })
}

/// Copies and validates one storm warning.
///
/// The phase is read first because it is what gives the number beside it a meaning: a
/// countdown, a remaining duration and a zero are three different quantities wearing one
/// field, so a phase this build cannot name leaves nothing to read `seconds_until` as.
///
/// `seconds_until` has no upper bound to check — the contract states none, deliberately —
/// and exactly one cross-field rule: a storm that has passed carries 0. That pair is
/// refused rather than repaired, for the reason a broken world clock is: the two halves
/// describe different storms and neither of them is knowably the server's.
fn storm_warning(warning: &fb::StormWarning<'_>) -> Result<StormWarning, DecodeError> {
    let phase = StormPhase::from_wire(warning.phase()).ok_or(DecodeError::UnknownStormPhase {
        value: warning.phase().0,
    })?;
    let seconds_until = warning.seconds_until();
    if phase == StormPhase::Passed && seconds_until != 0 {
        return Err(DecodeError::StormPassedWithCountdown { seconds_until });
    }
    Ok(StormWarning {
        seconds_until,
        phase,
    })
}

/// Copies and validates the complete set of warded columns in view.
///
/// **An empty set is accepted, and it is the message a player walking out of the last
/// ward receives.** That is why absence and empty are one case here — [`MarkerList`]'s
/// rule rather than [`MapExplored`]'s, because this replaces the client's copy instead of
/// adding to it, so an empty one states something.
///
/// The length is checked before anything is allocated from it, and column addresses are
/// checked for uniqueness because the set is complete: two rows for one column are two
/// answers about the same ground, and nothing downstream could tell which was meant.
fn wards_nearby(wards: &fb::WardsNearby<'_>) -> Result<WardsNearby, DecodeError> {
    let columns = wards.columns().unwrap_or_default();
    if columns.len() > MAX_WARDED_COLUMNS {
        return Err(DecodeError::TooManyWardedColumns(columns.len()));
    }

    let mut decoded = Vec::with_capacity(columns.len());
    let mut addresses = HashSet::new();
    for column in &columns {
        let (cx, cz) = (column.cx(), column.cz());
        let kind = WardKind::from_wire(column.kind()).ok_or(DecodeError::UnknownWardKind {
            cx,
            cz,
            value: column.kind().0,
        })?;
        if !addresses.insert((cx, cz)) {
            return Err(DecodeError::DuplicateWardedColumn { cx, cz });
        }
        // `mine` is copied verbatim and never checked against anything. It is
        // presentation: the server refuses a warded edit whatever this byte says, and a
        // client that read a permission out of it would be deciding a gameplay outcome.
        // A `Settlement` column claiming to be this player's is therefore a shading bug
        // on the server, not a frame this side may reinterpret.
        decoded.push(WardedColumn {
            cx,
            cz,
            kind,
            mine: column.mine(),
        });
    }
    Ok(WardsNearby { columns: decoded })
}

/// Copies and validates one relayed voice frame.
///
/// The speaker is read first because every other refusal names it, and the length is
/// checked before the bytes are copied — the ordering is the security property, the same
/// rule `frame::MAX_FRAME_SIZE` enforces on the length prefix one layer down.
///
/// **No refusal here may carry the payload**, and none does: a voice frame is personal
/// data that this side relays into a decoder and writes down nowhere.
fn voice_heard(heard: &fb::VoiceHeard<'_>) -> Result<VoiceHeard, DecodeError> {
    let speaker_entity_id = heard.speaker_entity_id();
    if speaker_entity_id == 0 {
        return Err(DecodeError::VoiceWithoutSpeaker);
    }

    // Absent and empty are the same nothing, and both are refused: the server drops a
    // frame with no audio in it rather than relaying one, so either shape is a peer
    // this client cannot take at its word.
    let opus = heard.opus().unwrap_or_default();
    if opus.is_empty() {
        return Err(DecodeError::VoiceWithoutAudio { speaker_entity_id });
    }
    if opus.len() > MAX_OPUS_BYTES {
        return Err(DecodeError::OversizedVoiceFrame {
            speaker_entity_id,
            len: opus.len(),
        });
    }

    Ok(VoiceHeard {
        speaker_entity_id,
        sequence: heard.sequence(),
        // Copied out like every other field here: the accessor borrows the frame, and
        // the frame is gone by the time anything reads this.
        opus: opus.bytes().to_vec(),
    })
}

/// Copies and validates the weather at the recipient's own position.
///
/// `None` in, `None` out: an absent struct is a server that keeps no weather, which a
/// test world and a pre-V26 server both legitimately are. A struct that is *present* has
/// to name a kind, because the absent case already has its own representation and a
/// second one spelled `Unknown` would be a defect wearing it.
fn weather_state(weather: Option<&fb::WeatherState>) -> Result<Option<WeatherState>, DecodeError> {
    let Some(weather) = weather else {
        return Ok(None);
    };
    let kind = WeatherKind::from_wire(weather.kind()).ok_or(DecodeError::UnknownWeatherKind {
        value: weather.kind().0,
    })?;
    let intensity = weather.intensity();
    if kind == WeatherKind::Clear && intensity != 0 {
        return Err(DecodeError::ClearWeatherWithIntensity { intensity });
    }
    Ok(Some(WeatherState { kind, intensity }))
}

/// Copies and validates one complete price list.
///
/// Both vectors must be present and at least one must be non-empty: a vendor with nothing
/// in either direction is `VendorClosed`, and reading an empty pair as an open-but-bare
/// stall would draw a window the server never opened.
///
/// Item ids are checked for uniqueness *within* a vector rather than across both, because
/// a vendor that sells iron at 12 and buys it at 5 is the ordinary case and the spread is
/// the whole of what a stall is. Two rows for one item in the *same* vector are two prices
/// with one address, and nothing downstream could tell which a `TradeRequest` meant.
fn vendor_state(state: &fb::VendorState<'_>) -> Result<VendorState, DecodeError> {
    let entity_id = state.entity_id();
    if entity_id == 0 {
        return Err(DecodeError::VendorWithoutEntity("VendorState"));
    }
    let revision = state.revision();
    if revision == 0 {
        return Err(DecodeError::VendorWithoutRevision);
    }

    let sells = vendor_entries(entity_id, state.sells(), "sells")?;
    let buys = vendor_entries(entity_id, state.buys(), "buys")?;
    if sells.is_empty() && buys.is_empty() {
        return Err(DecodeError::VendorWithNothingToTrade(entity_id));
    }

    Ok(VendorState {
        entity_id,
        revision,
        sells,
        buys,
    })
}

/// Copies and validates one direction of a price list.
///
/// `field` names the vector so that every refusal below says which of the two it came
/// from — the same reason [`DecodeError::MapTileArrayLength`] carries its field name.
fn vendor_entries(
    entity_id: u64,
    entries: Option<flatbuffers::Vector<'_, fb::VendorEntry>>,
    field: &'static str,
) -> Result<Vec<VendorEntry>, DecodeError> {
    // Absent is refused rather than read as empty: the contract requires both vectors
    // present, so an absent one is a message shape this contract does not have.
    let entries = entries.ok_or(DecodeError::VendorWithoutPrices { entity_id, field })?;

    let mut decoded = Vec::with_capacity(entries.len());
    let mut seen = HashSet::new();
    for entry in &entries {
        let item_id = entry.item_id();
        if item_id == 0 {
            return Err(DecodeError::VendorEntryWithoutItem { entity_id, field });
        }
        let price = entry.price();
        if price == 0 {
            return Err(DecodeError::VendorEntryWithoutPrice {
                entity_id,
                field,
                item_id,
            });
        }
        if !seen.insert(item_id) {
            return Err(DecodeError::DuplicateVendorEntry {
                entity_id,
                field,
                item_id,
            });
        }
        decoded.push(VendorEntry { item_id, price });
    }
    Ok(decoded)
}

/// Copies and validates one complete per-recipient player-trade state.
fn player_trade_state(state: &fb::PlayerTradeState<'_>) -> Result<PlayerTradeState, DecodeError> {
    let partner_entity_id = state.partner_entity_id();
    if partner_entity_id == 0 {
        return Err(DecodeError::PlayerTradeWithoutPartner("PlayerTradeState"));
    }
    let partner_name = state
        .partner_name()
        .ok_or(DecodeError::PlayerTradeWithoutPartnerName)?
        .to_owned();
    let revision = state.revision();
    if revision == 0 {
        return Err(DecodeError::PlayerTradeWithoutRevision);
    }

    let my_offer = player_trade_offer(
        state
            .my_offer()
            .ok_or(DecodeError::PlayerTradeWithoutOffer("my_offer"))?,
        "my_offer",
        false,
    )?;
    let their_offer = player_trade_offer(
        state
            .their_offer()
            .ok_or(DecodeError::PlayerTradeWithoutOffer("their_offer"))?,
        "their_offer",
        true,
    )?;

    Ok(PlayerTradeState {
        partner_entity_id,
        partner_name,
        revision,
        my_offer,
        their_offer,
        my_silver: state.my_silver(),
        their_silver: state.their_silver(),
        my_confirmed: state.my_confirmed(),
        their_confirmed: state.their_confirmed(),
    })
}

/// Copies one offer while holding its index, uniqueness and durability invariants.
fn player_trade_offer(
    offer: flatbuffers::Vector<'_, fb::PlayerTradeSlot>,
    field: &'static str,
    partner: bool,
) -> Result<Vec<PlayerTradeSlot>, DecodeError> {
    if offer.len() > PLAYER_TRADE_SLOTS {
        return Err(DecodeError::PlayerTradeOfferTooLarge {
            field,
            len: offer.len(),
        });
    }

    let mut decoded = Vec::with_capacity(offer.len());
    let mut seen = [false; PLAYER_TRADE_SLOTS];
    for (index, wire) in offer.iter().enumerate() {
        let slot = PlayerTradeSlot {
            trade_slot: wire.trade_slot(),
            pack_slot: wire.pack_slot(),
            item_id: wire.item_id(),
            count: wire.count(),
            durability: wire.durability(),
            max_durability: wire.max_durability(),
        };
        let trade_slot = usize::from(slot.trade_slot);
        if trade_slot >= PLAYER_TRADE_SLOTS {
            return Err(DecodeError::PlayerTradeSlotOutOfRange {
                field,
                index,
                trade_slot: slot.trade_slot,
            });
        }
        if seen[trade_slot] {
            return Err(DecodeError::DuplicatePlayerTradeSlot {
                field,
                trade_slot: slot.trade_slot,
            });
        }
        if slot.count == 0 {
            return Err(DecodeError::EmptyPlayerTradeSlot { field, index });
        }
        if slot.max_durability == 0 && slot.durability != 0 {
            return Err(DecodeError::PlayerTradeDurabilityWithoutMaximum {
                field,
                index,
                durability: slot.durability,
            });
        }
        if slot.durability > slot.max_durability {
            return Err(DecodeError::PlayerTradeDurabilityExceedsMaximum {
                field,
                index,
                durability: slot.durability,
                max_durability: slot.max_durability,
            });
        }
        if slot.max_durability != 0 && slot.count != 1 {
            return Err(DecodeError::PlayerTradeDurableStackCount {
                field,
                index,
                count: slot.count,
            });
        }
        if partner && slot.pack_slot != 0 {
            return Err(DecodeError::PlayerTradePartnerPackSlot {
                index,
                pack_slot: slot.pack_slot,
            });
        }
        seen[trade_slot] = true;
        decoded.push(slot);
    }
    Ok(decoded)
}

/// Copies and validates one complete per-recipient corpse container.
fn loot_state(state: &fb::LootState<'_>) -> Result<LootState, DecodeError> {
    let corpse_id = state.corpse_id();
    if corpse_id == 0 {
        return Err(DecodeError::LootWithoutCorpse("LootState"));
    }
    let revision = state.revision();
    if revision == 0 {
        return Err(DecodeError::LootWithoutRevision);
    }
    let entries = state
        .entries()
        .ok_or(DecodeError::LootWithoutEntries(corpse_id))?;
    if entries.is_empty() && state.silver() == 0 {
        return Err(DecodeError::LootWithoutEntries(corpse_id));
    }

    let mut decoded = Vec::with_capacity(entries.len());
    let mut identities = HashSet::new();
    for entry in &entries {
        let entry_id = entry.entry_id();
        if entry_id == 0 {
            return Err(DecodeError::LootEntryWithoutIdentity(corpse_id));
        }
        if !identities.insert(entry_id) {
            return Err(DecodeError::DuplicateLootEntry {
                corpse_id,
                entry_id,
            });
        }
        let item_id = entry.item_id();
        let count = entry.count();
        let durability = entry.durability();
        let max_durability = entry.max_durability();
        let invalid = item_id == 0
            || count == 0
            || if max_durability == 0 {
                durability != 0
            } else {
                durability > max_durability || count != 1
            };
        if invalid {
            return Err(DecodeError::InvalidLootEntry {
                corpse_id,
                entry_id,
                item_id,
                count,
                durability,
                max_durability,
            });
        }
        decoded.push(LootEntry {
            entry_id,
            item_id,
            count,
            durability,
            max_durability,
        });
    }
    Ok(LootState {
        corpse_id,
        revision,
        entries: decoded,
        silver: state.silver(),
    })
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
    Ok(InventoryState {
        stacks,
        silver: state.silver(),
    })
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
            if !player_ids.insert(state.entity_id) {
                return Err(DecodeError::DuplicateEntity(state.entity_id));
            }
            entities.push(state);
        }
    }

    let mut mounts = Vec::new();
    let mut mounted_ids = HashSet::new();
    if let Some(list) = snapshot.mounts() {
        mounts.reserve(list.len());
        for state in list.iter() {
            let entity_id = state.entity_id();
            if !player_ids.contains(&entity_id) {
                return Err(DecodeError::MountNotInSnapshot(entity_id));
            }
            if !mounted_ids.insert(entity_id) {
                return Err(DecodeError::DuplicateMountState(entity_id));
            }
            let mount =
                MountKind::from_wire(state.mount()).ok_or(DecodeError::UnknownMountKind {
                    entity_id,
                    value: state.mount().0,
                })?;
            mounts.push(MountState { entity_id, mount });
        }
    }

    let self_cast = if let Some(state) = snapshot.self_cast() {
        let kind = CastKind::from_wire(state.kind())
            .ok_or(DecodeError::UnknownCastKind(state.kind().0))?;
        let progress = state.progress();
        if progress == u8::MAX {
            return Err(DecodeError::CompletedCast);
        }
        Some(CastState { kind, progress })
    } else {
        None
    };

    let mut drops = Vec::new();
    let mut drop_indexes = HashMap::new();
    let mut mob_ids = HashSet::new();
    if let Some(list) = snapshot.drops() {
        drops.reserve(list.len());
        for state in &list {
            let state = item_drop_state(state)?;
            if player_ids.contains(&state.entity_id) {
                return Err(DecodeError::PlayerDropEntityConflict(state.entity_id));
            }
            if drop_indexes.insert(state.entity_id, drops.len()).is_some() {
                return Err(DecodeError::DuplicateDrop(state.entity_id));
            }
            drops.push(state);
        }
    }

    // Wear is keyed back onto the fixed drop vector by the server-minted identity.
    // Absence is the common, wearless case; a malformed association refuses the whole
    // snapshot rather than guessing which visible object the values describe.
    let mut durability_ids = HashSet::new();
    if let Some(list) = snapshot.drop_durabilities() {
        for wear in &list {
            let entity_id = wear.entity_id();
            let Some(&index) = drop_indexes.get(&entity_id) else {
                return Err(DecodeError::DropDurabilityWithoutDrop(entity_id));
            };
            if !durability_ids.insert(entity_id) {
                return Err(DecodeError::DropDurabilityNamedTwice(entity_id));
            }

            let durability = wear.durability();
            let max_durability = wear.max_durability();
            let count = drops[index].count;
            if max_durability == 0 || durability > max_durability || count != 1 {
                return Err(DecodeError::DropDurability {
                    entity_id,
                    count,
                    durability,
                    max_durability,
                });
            }
            drops[index].durability = durability;
            drops[index].max_durability = max_durability;
        }
    }

    let mut projectiles = Vec::new();
    let mut projectile_ids = HashSet::new();
    if let Some(list) = snapshot.projectiles() {
        projectiles.reserve(list.len());
        for state in &list {
            let state = projectile_state(state)?;
            if !projectile_ids.insert(state.entity_id) {
                return Err(DecodeError::DuplicateProjectile(state.entity_id));
            }
            projectiles.push(state);
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
                || drop_indexes.contains_key(&state.entity_id)
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
                || drop_indexes.contains_key(&state.structure_id)
                || mob_ids.contains(&state.structure_id)
                || !structure_ids.insert(state.structure_id)
            {
                return Err(DecodeError::StructureEntityConflict(state.structure_id));
            }
            structures.push(state);
        }
    }

    // **Read against `entities` rather than beside it**, which is what stops the two vectors
    // from disagreeing. Both cases are refused rather than skipped, for the reason the id
    // conflicts above are: a frame that breaks its own stated invariant is a bug reporting
    // itself, and a receiver that quietly repaired it would hide the only evidence.
    let mut dead_players = Vec::new();
    let mut dead_ids = HashSet::new();
    if let Some(list) = snapshot.dead_players() {
        dead_players.reserve(list.len());
        for entity_id in list.iter() {
            if !player_ids.contains(&entity_id) {
                return Err(DecodeError::DeadPlayerNotInSnapshot(entity_id));
            }
            if !dead_ids.insert(entity_id) {
                return Err(DecodeError::DeadPlayerNamedTwice(entity_id));
            }
            dead_players.push(entity_id);
        }
    }

    let mut blocking_players = Vec::new();
    let mut blocking_ids = HashSet::new();
    if let Some(list) = snapshot.blocking_players() {
        blocking_players.reserve(list.len());
        for entity_id in list.iter() {
            if !player_ids.contains(&entity_id) {
                return Err(DecodeError::BlockingPlayerNotInSnapshot(entity_id));
            }
            if !blocking_ids.insert(entity_id) {
                return Err(DecodeError::BlockingPlayerNamedTwice(entity_id));
            }
            blocking_players.push(entity_id);
        }
    }

    let party_leader_entity_id = snapshot.party_leader_entity_id();
    let mut party_members = Vec::new();
    let mut party_member_ids = HashSet::new();
    if let Some(list) = snapshot.party_members() {
        party_members.reserve(list.len());
        for member in &list {
            let entity_id = member.entity_id();
            if entity_id == 0 {
                return Err(DecodeError::PartyMemberWithoutIdentity);
            }
            if !party_member_ids.insert(entity_id) {
                return Err(DecodeError::DuplicatePartyMember(entity_id));
            }
            let pos = member.pos();
            let checked = |field: &'static str, value: f32| {
                if value.is_finite() {
                    Ok(value)
                } else {
                    Err(DecodeError::NonFinitePartyMember {
                        entity_id,
                        field,
                        value,
                    })
                }
            };
            let health = member.health();
            let max_health = member.max_health();
            if max_health == 0 || health > max_health {
                return Err(DecodeError::PartyMemberHealth {
                    entity_id,
                    health,
                    max_health,
                });
            }
            party_members.push(PartyMemberState {
                entity_id,
                pos: [
                    checked("pos.x", pos.x())?,
                    checked("pos.y", pos.y())?,
                    checked("pos.z", pos.z())?,
                ],
                health,
                max_health,
                alive: member.alive(),
            });
        }
    }
    let mut party_roster = Vec::new();
    let mut character_ids = HashSet::new();
    let mut roster_entity_ids = HashSet::new();
    if let Some(list) = snapshot.party_roster() {
        party_roster.reserve(list.len());
        for member in list.iter() {
            let character_id = member.character_id();
            if character_id == 0 {
                return Err(DecodeError::PartyRosterWithoutCharacter);
            }
            if !character_ids.insert(character_id) {
                return Err(DecodeError::DuplicatePartyCharacter(character_id));
            }
            let entity_id = member.entity_id();
            let online = member.online();
            if online != (entity_id != 0) {
                return Err(DecodeError::PartyRosterOnlineMismatch {
                    character_id,
                    entity_id,
                    online,
                });
            }
            if entity_id != 0 && !roster_entity_ids.insert(entity_id) {
                return Err(DecodeError::DuplicatePartyRosterEntity(entity_id));
            }
            let name = member
                .name()
                .ok_or(DecodeError::PartyRosterWithoutName(character_id))?
                .to_owned();
            party_roster.push(PartyRosterMember {
                character_id,
                entity_id,
                name,
                online,
            });
        }
    }

    if party_roster.is_empty() {
        if party_leader_entity_id != 0 || !party_members.is_empty() {
            return Err(DecodeError::PartyMembersWithoutLeader);
        }
    } else {
        let expected_leader = party_roster[0].entity_id;
        if party_leader_entity_id != expected_leader {
            return Err(DecodeError::PartyLeaderRosterMismatch {
                expected: expected_leader,
                actual: party_leader_entity_id,
            });
        }
        for member in &party_members {
            if !roster_entity_ids.contains(&member.entity_id) {
                return Err(DecodeError::PartyMemberMissingFromRoster(member.entity_id));
            }
        }
    }

    let mut accessible_loot_corpses = Vec::new();
    let mut accessible_ids = HashSet::new();
    if let Some(list) = snapshot.accessible_loot_corpses() {
        accessible_loot_corpses.reserve(list.len());
        for corpse_id in list.iter() {
            if corpse_id == 0 {
                return Err(DecodeError::AccessibleCorpseWithoutIdentity);
            }
            if !accessible_ids.insert(corpse_id) {
                return Err(DecodeError::DuplicateAccessibleCorpse(corpse_id));
            }
            if !mobs
                .iter()
                .any(|mob| mob.entity_id == corpse_id && mob.action == MobAction::Corpse)
            {
                return Err(DecodeError::AccessibleCorpseWithoutMob(corpse_id));
            }
            accessible_loot_corpses.push(corpse_id);
        }
    }

    Ok(Snapshot {
        server_tick: snapshot.server_tick(),
        entities,
        drops,
        projectiles,
        mobs,
        mounts,
        self_vitals: player_vitals(&snapshot.self_vitals())?,
        self_cast,
        structures,
        // Copied, not checked. See the field's own documentation: the bound is against a
        // number this function has never seen.
        tick_of_day: snapshot.tick_of_day(),
        world_tick: snapshot.world_tick(),
        dead_players,
        blocking_players,
        party_leader_entity_id,
        party_members,
        party_roster,
        accessible_loot_corpses,
        weather: weather_state(snapshot.weather())?,
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
        // Copied, never checked, and there is nothing here to check it against: `lit` is
        // a bool, so every value it can hold is a legal one, and its cross-field rule —
        // that it means anything only for a campfire — is the reader's to honour rather
        // than the decoder's to enforce. A pre-V26 server writes no byte at all and the
        // contract's `true` default answers for it, which is the whole reason the default
        // is `true`: the elided field reads as the burning fire every fire on such a
        // server is.
        lit: state.lit(),
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
        target_entity_id: state.target_entity_id(),
    })
}

/// Copies the recipient's own vitals out of a snapshot.
///
/// The field is `(required)` on the wire, so an absent one never reaches here: the
/// verifier [`decode`] runs has already refused the buffer. What is left are the value
/// invariants, and they are the ones any vitals presentation depends on: all three
/// denominators are non-zero and no peer-owned current value may exceed its maximum.
fn player_vitals(vitals: &fb::PlayerVitals) -> Result<PlayerVitals, DecodeError> {
    let life_state =
        LifeState::from_wire(vitals.life_state()).ok_or(DecodeError::UnknownLifeState)?;

    let (health, max_health) = (vitals.health(), vitals.max_health());
    if max_health == 0 || health > max_health {
        return Err(DecodeError::VitalsHealth { health, max_health });
    }
    let (hunger, max_hunger) = (vitals.hunger(), vitals.max_hunger());
    if max_hunger == 0 || hunger > max_hunger {
        return Err(DecodeError::VitalsHunger { hunger, max_hunger });
    }
    let (level, experience, experience_to_next) = (
        vitals.level(),
        vitals.experience(),
        vitals.experience_to_next(),
    );
    if level == 0 || experience_to_next == 0 || experience > experience_to_next {
        return Err(DecodeError::VitalsExperience {
            level,
            experience,
            experience_to_next,
        });
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
        hunger,
        max_hunger,
        level,
        experience,
        experience_to_next,
        life_state,
        respawn_ticks,
        invulnerable: vitals.invulnerable(),
        blocking: vitals.blocking(),
    })
}

/// Copies one entity out of a snapshot, refusing a non-finite component.
///
/// A finiteness test, never a clamp. NaN compares false against every bound, so a clamp
/// would pass one through untouched — into the interpolation, then into a `Transform`,
/// and from there into every child of it.
fn entity_state(state: &fb::EntityState) -> Result<EntityState, DecodeError> {
    let entity_id = state.entity_id();
    if entity_id == 0 {
        return Err(DecodeError::EntityWithoutIdentity);
    }
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
        durability: 0,
        max_durability: 0,
    })
}

/// Copies one projectile out of a snapshot and enforces every invariant attached
/// to `ProjectileState` in the schema before a component reaches interpolation.
fn projectile_state(state: &fb::ProjectileState) -> Result<ProjectileState, DecodeError> {
    let entity_id = state.entity_id();
    let checked = |field: &'static str, value: f32| -> Result<f32, DecodeError> {
        if value.is_finite() {
            Ok(value)
        } else {
            Err(DecodeError::NonFiniteProjectile {
                entity_id,
                field,
                value,
            })
        }
    };

    let pos = state.pos();
    let vel = state.vel();
    let kind =
        ProjectileKind::from_wire(state.kind()).ok_or(DecodeError::UnknownProjectileKind {
            entity_id,
            value: state.kind().0,
        })?;

    Ok(ProjectileState {
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
        kind,
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
    let equipment_slots = welcome.equipment_slots();
    if !(1..=MAX_EQUIPMENT_SLOTS).contains(&equipment_slots) {
        return Err(DecodeError::EquipmentSlots(equipment_slots));
    }
    if u16::from(hotbar_slots) + u16::from(equipment_slots) > u16::from(inventory_slots) {
        return Err(DecodeError::ReservedSlotsExceedInventory {
            hotbar: hotbar_slots,
            equipment: equipment_slots,
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

    // Finite and not negative, checked the way a spawn axis is and for the same reason:
    // NaN compares false against every bound, so a clamp would pass it through untouched
    // and it would then propagate into whatever attenuation reads it. Zero is not a
    // degenerate radius — it is a server that relays no voice — so it is accepted here
    // and asked about at the point of use, exactly as `WorldClock::declared` is.
    let voice_range_blocks = welcome.voice_range_blocks();
    if !voice_range_blocks.is_finite() || voice_range_blocks < 0.0 {
        return Err(DecodeError::VoiceRange(voice_range_blocks));
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
        equipment_slots,
        player_token: PlayerToken::from_bytes(player_token),
        clock,
        voice_range_blocks,
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

/// Builds one chat intent. The text is copied verbatim, empty and whitespace included;
/// the authoritative server owns length and rate decisions.
pub fn encode_chat_request(request: &ChatRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let text = builder.create_string(&request.text);
    let payload = fb::ChatRequest::create(&mut builder, &fb::ChatRequestArgs { text: Some(text) });
    finish_envelope(builder, fb::Payload::ChatRequest, payload.as_union_value())
}

/// Builds one party intent. Target display text is copied verbatim and is meaningful
/// only to the server for Invite and Kick.
pub fn encode_party_request(request: &PartyRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let target_name = builder.create_string(&request.target_name);
    let payload = fb::PartyRequest::create(
        &mut builder,
        &fb::PartyRequestArgs {
            action: request.action.wire(),
            target_name: Some(target_name),
        },
    );
    finish_envelope(builder, fb::Payload::PartyRequest, payload.as_union_value())
}

/// Builds one corpse-open intent. It carries identity and client ordering only.
// V21 establishes this outbound contract before the loot interaction UI lands.
#[allow(dead_code)]
pub fn encode_loot_open_request(request: &LootOpenRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::LootOpenRequest::create(
        &mut builder,
        &fb::LootOpenRequestArgs {
            corpse_id: request.corpse_id,
            client_tick: request.client_tick,
        },
    );
    finish_envelope(
        builder,
        fb::Payload::LootOpenRequest,
        payload.as_union_value(),
    )
}

/// Builds one stable-entry take intent. Stack contents and inventory outcome stay absent.
// V21 establishes this outbound contract before the loot interaction UI lands.
#[allow(dead_code)]
pub fn encode_loot_take_request(request: &LootTakeRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::LootTakeRequest::create(
        &mut builder,
        &fb::LootTakeRequestArgs {
            corpse_id: request.corpse_id,
            entry_id: request.entry_id,
            revision: request.revision,
            client_tick: request.client_tick,
        },
    );
    finish_envelope(
        builder,
        fb::Payload::LootTakeRequest,
        payload.as_union_value(),
    )
}

/// Builds one take-everything intent, from the revision currently on screen.
///
/// The revision is what makes this safe to originate from a view one message old: a
/// container that changed since answers `StaleRevision` rather than emptying something
/// the player never saw.
pub fn encode_loot_take_all_request(request: &LootTakeAllRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::LootTakeAllRequest::create(
        &mut builder,
        &fb::LootTakeAllRequestArgs {
            corpse_id: request.corpse_id,
            revision: request.revision,
            client_tick: request.client_tick,
        },
    );
    finish_envelope(
        builder,
        fb::Payload::LootTakeAllRequest,
        payload.as_union_value(),
    )
}

/// Builds one map-tile request. It states nothing about the ground or about what this
/// character has explored; the server owns both.
///
/// The origin and scale are written exactly as given, misaligned ones included. This is
/// an encoder, not a validator: the grid is the decode boundary's to hold, and a client
/// that corrected its own request here would hide the bug that produced it.
// V24 establishes this outbound contract before the map window lands.
#[allow(dead_code)]
pub fn encode_map_tile_request(request: &MapTileRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::MapTileRequest::create(
        &mut builder,
        &fb::MapTileRequestArgs {
            origin_x: request.origin_x,
            origin_z: request.origin_z,
            scale: request.scale,
            client_tick: request.client_tick,
        },
    );
    finish_envelope(
        builder,
        fb::Payload::MapTileRequest,
        payload.as_union_value(),
    )
}

/// Builds one mark-placement intent. It carries no marker id, because identity is the
/// server's to mint, and the note is copied verbatim for the server to bound.
pub fn encode_marker_place_request(request: &MarkerPlaceRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY * 2);
    let note = builder.create_string(&request.note);
    let payload = fb::MarkerPlaceRequest::create(
        &mut builder,
        &fb::MarkerPlaceRequestArgs {
            x: request.x,
            z: request.z,
            kind: request.kind.wire(),
            note: Some(note),
            client_tick: request.client_tick,
        },
    );
    finish_envelope(
        builder,
        fb::Payload::MarkerPlaceRequest,
        payload.as_union_value(),
    )
}

/// Builds one mark-removal intent. There is no edit message: a change is a removal and
/// a placement, and both are answered by the same complete `MarkerList`.
pub fn encode_marker_remove_request(request: &MarkerRemoveRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::MarkerRemoveRequest::create(
        &mut builder,
        &fb::MarkerRemoveRequestArgs {
            marker_id: request.marker_id,
            client_tick: request.client_tick,
        },
    );
    finish_envelope(
        builder,
        fb::Payload::MarkerRemoveRequest,
        payload.as_union_value(),
    )
}

/// Builds one intent to address a resident.
///
/// It carries the entity and the client's tick and nothing else — no distance, no shop,
/// no outcome. Whether anything opens is the server's answer, arriving as a
/// `VendorState` or an `ActionRefused`.
// V25 establishes this outbound contract before the resident interaction lands.
#[allow(dead_code)]
pub fn encode_npc_interact_request(request: &NpcInteractRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::NpcInteractRequest::create(
        &mut builder,
        &fb::NpcInteractRequestArgs {
            entity_id: request.entity_id,
            client_tick: request.client_tick,
        },
    );
    finish_envelope(
        builder,
        fb::Payload::NpcInteractRequest,
        payload.as_union_value(),
    )
}

/// Builds one trade intent, from the price list currently on screen.
///
/// The revision is what makes this safe to originate from a view one message old: a list
/// that changed since is refused rather than applied to prices the player never saw. No
/// price and no total are written, and there are no fields to write them into — the
/// contract has none, deliberately.
// V25 establishes this outbound contract before the vendor window lands.
#[allow(dead_code)]
pub fn encode_trade_request(request: &TradeRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::TradeRequest::create(
        &mut builder,
        &fb::TradeRequestArgs {
            entity_id: request.entity_id,
            item_id: request.item_id,
            count: request.count,
            buying: request.buying,
            revision: request.revision,
            client_tick: request.client_tick,
        },
    );
    finish_envelope(builder, fb::Payload::TradeRequest, payload.as_union_value())
}

/// Builds intent verbatim. The action decides which fields the server reads; an
/// offered item is only a pack slot, never client-stated stack contents.
pub fn encode_player_trade_request(request: &PlayerTradeRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::PlayerTradeRequest::create(
        &mut builder,
        &fb::PlayerTradeRequestArgs {
            action: request.action.wire(),
            target_entity_id: request.target_entity_id,
            trade_slot: request.trade_slot,
            pack_slot: request.pack_slot,
            silver: request.silver,
            revision: request.revision,
            client_tick: request.client_tick,
        },
    );
    finish_envelope(
        builder,
        fb::Payload::PlayerTradeRequest,
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

/// Builds one shield-intent edge.
pub fn encode_block_request(request: &BlockRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let mut table = fb::BlockRequestBuilder::new(&mut builder);
    table.add_active(request.active);
    table.add_client_tick(request.client_tick);
    let payload = table.finish();
    finish_envelope(builder, fb::Payload::BlockRequest, payload.as_union_value())
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

/// Builds one consume intent.
///
/// The whole message is one authoritative slot index and this client's tick counter.
/// There is no item id or restored amount: both come from server-owned state, and an
/// ineligible item, a full reserve or a dead player is answered with silence.
///
/// The index is sent exactly as given, including values outside the announced pack,
/// because the simulation treats those as gameplay refusals rather than malformed frames.
pub fn encode_consume_request(request: &ConsumeRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::ConsumeRequestBuilder::new(&mut builder);
    table.add_slot(request.slot);
    table.add_client_tick(request.client_tick);
    let payload = table.finish();

    finish_envelope(
        builder,
        fb::Payload::ConsumeRequest,
        payload.as_union_value(),
    )
}

/// Builds one drop intent.
///
/// The whole message: one authoritative slot index and this client's own tick counter. No
/// count, because the contract has no field for one — a whole stack is what leaves, and how
/// much that is is read from the slot server-side. No position either: the stack lands where
/// the *server* says the player's feet are.
///
/// The index is sent exactly as given, out-of-range values included, because
/// `schemas/player.fbs` asks for that. A refused drop is silence.
pub fn encode_drop_item_request(request: &DropItemRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let mut table = fb::DropItemRequestBuilder::new(&mut builder);
    table.add_slot(request.slot);
    table.add_client_tick(request.client_tick);
    let payload = table.finish();

    finish_envelope(
        builder,
        fb::Payload::DropItemRequest,
        payload.as_union_value(),
    )
}

/// Builds the empty leave intent. The missing duration and cancellation are the point:
/// this client asks, and the server answers how long the body remains.
pub fn encode_leave_request() -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let payload = fb::LeaveRequest::create(&mut builder, &fb::LeaveRequestArgs::default());
    finish_envelope(builder, fb::Payload::LeaveRequest, payload.as_union_value())
}

/// Builds the empty request to stop a live leave countdown.
///
/// There is no deadline or result for this client to state. The server compares the
/// request with its own countdown and answers with [`LeaveCancelResult`].
pub fn encode_leave_cancel_request() -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);

    let payload =
        fb::LeaveCancelRequest::create(&mut builder, &fb::LeaveCancelRequestArgs::default());
    finish_envelope(
        builder,
        fb::Payload::LeaveCancelRequest,
        payload.as_union_value(),
    )
}

/// Builds one request to begin the server-owned cast for a learned mount.
pub fn encode_mount_request(request: &MountRequest) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::MountRequest::create(
        &mut builder,
        &fb::MountRequestArgs {
            mount: request.mount.wire(),
        },
    );
    finish_envelope(builder, fb::Payload::MountRequest, payload.as_union_value())
}

/// Builds the deliberately empty immediate dismount intent.
pub fn encode_dismount_request() -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(BUILDER_CAPACITY);
    let payload = fb::DismountRequest::create(&mut builder, &fb::DismountRequestArgs::default());
    finish_envelope(
        builder,
        fb::Payload::DismountRequest,
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
    use super::{FlatBufferBuilder, LootEntry, MAX_CHUNK_SIZE, WorldClock, fb, finish_envelope};

    /// The token [`WelcomeWire::default`] carries: a legal one, so a test that is
    /// not about identity never has to name it.
    pub const DEFAULT_TOKEN: [u8; super::PLAYER_TOKEN_LEN] = [0x5a; super::PLAYER_TOKEN_LEN];

    /// Test-server offered stack, including invalid combinations.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PlayerTradeSlotWire {
        pub trade_slot: u8,
        pub pack_slot: u8,
        pub item_id: u16,
        pub count: u16,
        pub durability: u16,
        pub max_durability: u16,
    }

    impl Default for PlayerTradeSlotWire {
        fn default() -> Self {
            Self {
                trade_slot: 0,
                pack_slot: 7,
                item_id: 31,
                count: 1,
                durability: 0,
                max_durability: 0,
            }
        }
    }

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
        pub equipment_slots: u8,
        /// `None` omits the field; any other length than
        /// [`super::PLAYER_TOKEN_LEN`] is a token the decoder must refuse, the
        /// empty vector included.
        pub player_token: Option<Vec<u8>>,
        /// The three clock scalars, written verbatim and never validated here: this
        /// helper is how a *peer* emits a welcome, including one whose clock is
        /// nonsense, which is the case the decoder exists for.
        pub clock: WorldClock,
        /// Written verbatim for the same reason, negative and non-finite included.
        pub voice_range_blocks: f32,
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
                inventory_slots: 40,
                hotbar_slots: 9,
                equipment_slots: 4,
                player_token: Some(DEFAULT_TOKEN.to_vec()),
                // No clock by default, which is what every server in this repository
                // announces today and therefore the shape most fixtures should carry.
                clock: WorldClock::default(),
                // No voice by default, which is the pre-V30 shape an absent scalar
                // decodes to and what a fixture that says nothing about voice means.
                voice_range_blocks: 0.0,
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
        table.add_equipment_slots(welcome.equipment_slots);
        table.add_voice_range_blocks(welcome.voice_range_blocks);
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
                silver: 0,
            },
        );
        finish_envelope(
            builder,
            fb::Payload::InventoryState,
            inventory.as_union_value(),
        )
    }

    pub fn encode_empty_inventory_with_silver(silver: u32) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::new();
        let empty = builder.create_vector::<u16>(&[]);
        let inventory = fb::InventoryState::create(
            &mut builder,
            &fb::InventoryStateArgs {
                stacks: Some(empty),
                durability: Some(empty),
                max_durability: Some(empty),
                silver,
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
        pub durability: u16,
        pub max_durability: u16,
    }

    impl ItemDropStateWire {
        /// A valid drop at a useful non-origin position, for a test to break one field of.
        pub fn item(entity_id: u64, item_id: u16) -> Self {
            Self {
                entity_id,
                pos: [1.5, 64.25, -2.0],
                item_id,
                count: 1,
                durability: 0,
                max_durability: 0,
            }
        }
    }

    /// One projectile as it sits on the wire, before validation.
    #[derive(Debug, Clone, Copy)]
    pub struct ProjectileStateWire {
        pub entity_id: u64,
        pub pos: [f32; 3],
        pub vel: [f32; 3],
        pub kind: fb::ProjectileKind,
    }

    impl ProjectileStateWire {
        pub fn arrow(entity_id: u64, x: f32) -> Self {
            Self {
                entity_id,
                pos: [x, 64.0, -2.0],
                vel: [28.0, -3.0, 0.0],
                kind: fb::ProjectileKind::Arrow,
            }
        }
    }

    /// One sparse durability entry as it sits beside the fixed drop vector.
    /// Kept separate so decoder tests can construct orphaned and repeated entries.
    #[derive(Debug, Clone, Copy)]
    pub struct ItemDropDurabilityWire {
        pub entity_id: u64,
        pub durability: u16,
        pub max_durability: u16,
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
        pub target_entity_id: u64,
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
                target_entity_id: 0,
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
        /// Written unconditionally by the builder below, `true` included, so that a test
        /// naming `lit: true` produces a frame that carries the byte rather than one that
        /// elides it. The elided case is its own frame and its own encoder.
        pub lit: bool,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct PartyMemberStateWire {
        pub entity_id: u64,
        pub pos: [f32; 3],
        pub health: u16,
        pub max_health: u16,
        pub alive: bool,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct PartyRosterMemberWire<'a> {
        pub character_id: u64,
        pub entity_id: u64,
        pub name: Option<&'a str>,
        pub online: bool,
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
                lit: true,
            }
        }
    }

    /// The recipient's vitals as they sit on the wire, before validation.
    #[derive(Debug, Clone, Copy)]
    pub struct PlayerVitalsWire {
        pub health: u16,
        pub max_health: u16,
        pub hunger: u16,
        pub max_hunger: u16,
        pub level: u16,
        pub experience: u32,
        pub experience_to_next: u32,
        pub life_state: fb::LifeState,
        pub respawn_ticks: u32,
        pub invulnerable: bool,
        pub blocking: bool,
    }

    impl Default for PlayerVitalsWire {
        /// An unharmed living player, so a test only names the field it is breaking.
        fn default() -> Self {
            Self {
                health: 100,
                max_health: 100,
                hunger: 100,
                max_hunger: 100,
                level: 1,
                experience: 0,
                experience_to_next: 50,
                life_state: fb::LifeState::Alive,
                respawn_ticks: 0,
                invulnerable: false,
                blocking: false,
            }
        }
    }

    /// Encodes an `EntitySnapshot` envelope. Mirrors `protocol.EncodeEntitySnapshot`,
    /// including the back-to-front vector build a struct vector needs.
    pub fn encode_entity_snapshot(server_tick: u32, entities: &[EntityStateWire]) -> Vec<u8> {
        encode_entity_snapshot_with_drops(server_tick, entities, &[])
    }

    /// Builds the V27 sparse mount projection and recipient-only cast with raw enum
    /// values, including states a correct server cannot emit.
    pub fn encode_snapshot_with_mounts(
        entities: &[EntityStateWire],
        mounts: &[(u64, fb::MountKind)],
        self_cast: Option<(fb::CastKind, u8)>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::new();
        let entities: Vec<_> = entities
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
        let entities = builder.create_vector(&entities);
        let mounts: Vec<_> = mounts
            .iter()
            .map(|(entity_id, mount)| {
                fb::MountState::create(
                    &mut builder,
                    &fb::MountStateArgs {
                        entity_id: *entity_id,
                        mount: *mount,
                    },
                )
            })
            .collect();
        let mounts = builder.create_vector(&mounts);
        let self_cast = self_cast.map(|(kind, progress)| {
            fb::CastState::create(&mut builder, &fb::CastStateArgs { kind, progress })
        });
        let vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: 100,
                max_health: 100,
                hunger: 100,
                max_hunger: 100,
                level: 1,
                experience_to_next: 50,
                life_state: fb::LifeState::Alive,
                ..Default::default()
            },
        );
        let mut snapshot = fb::EntitySnapshotBuilder::new(&mut builder);
        snapshot.add_server_tick(1);
        snapshot.add_entities(entities);
        snapshot.add_mounts(mounts);
        snapshot.add_self_vitals(vitals);
        if let Some(self_cast) = self_cast {
            snapshot.add_self_cast(self_cast);
        }
        let snapshot = snapshot.finish();
        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            snapshot.as_union_value(),
        )
    }

    pub fn encode_learned_mounts(mounts: Option<&[fb::MountKind]>) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::new();
        let mounts = mounts.map(|mounts| builder.create_vector(mounts));
        let payload = fb::LearnedMounts::create(&mut builder, &fb::LearnedMountsArgs { mounts });
        finish_envelope(
            builder,
            fb::Payload::LearnedMounts,
            payload.as_union_value(),
        )
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

    /// Encodes the append-only projectile vector independently, including malformed
    /// values a correct server never emits.
    pub fn encode_entity_snapshot_with_projectiles(
        server_tick: u32,
        projectiles: &[ProjectileStateWire],
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(projectiles.len() * 40 + 128);
        let laid_out: Vec<fb::ProjectileState> = projectiles
            .iter()
            .map(|state| {
                fb::ProjectileState::new(
                    state.entity_id,
                    &fb::Vec3::new(state.pos[0], state.pos[1], state.pos[2]),
                    &fb::Vec3::new(state.vel[0], state.vel[1], state.vel[2]),
                    state.kind,
                )
            })
            .collect();
        let projectiles = builder.create_vector(&laid_out);
        let vitals = PlayerVitalsWire::default();
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                hunger: vitals.hunger,
                max_hunger: vitals.max_hunger,
                level: vitals.level,
                experience: vitals.experience,
                experience_to_next: vitals.experience_to_next,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
                blocking: false,
            },
        );

        let mut table = fb::EntitySnapshotBuilder::new(&mut builder);
        table.add_server_tick(server_tick);
        table.add_self_vitals(self_vitals);
        table.add_projectiles(projectiles);
        let payload = table.finish();
        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            payload.as_union_value(),
        )
    }

    /// Encodes a snapshot whose sparse wear vector is supplied independently.
    /// Production-shaped test frames use `encode_entity_snapshot_with`; this seam exists
    /// for associations a correct server cannot produce.
    pub fn encode_entity_snapshot_with_drop_durabilities(
        server_tick: u32,
        drops: &[ItemDropStateWire],
        durabilities: &[ItemDropDurabilityWire],
    ) -> Vec<u8> {
        encode_entity_snapshot_with_explicit_durabilities(
            server_tick,
            &[],
            drops,
            &[],
            PlayerVitalsWire::default(),
            &[],
            durabilities,
            0,
            &[],
        )
    }

    /// Encodes an `EntitySnapshot` naming some of its entities dead.
    ///
    /// Its own builder rather than a seventh parameter on the one below, whose eleven
    /// callers would all have gained a `&[]`. `dead_players` is settable to states no server
    /// would emit — an id outside `entities`, or one named twice — because those are the
    /// frames the decoder's invariants exist for.
    pub fn encode_entity_snapshot_with_dead(
        server_tick: u32,
        entities: &[EntityStateWire],
        vitals: PlayerVitalsWire,
        dead_players: &[u64],
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(entities.len() * 40 + 128);

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
        let dead = builder.create_vector(dead_players);
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                hunger: vitals.hunger,
                max_hunger: vitals.max_hunger,
                level: vitals.level,
                experience: vitals.experience,
                experience_to_next: vitals.experience_to_next,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
                blocking: vitals.blocking,
            },
        );

        let mut table = fb::EntitySnapshotBuilder::new(&mut builder);
        table.add_server_tick(server_tick);
        table.add_entities(entities);
        table.add_self_vitals(self_vitals);
        table.add_dead_players(dead);
        let payload = table.finish();

        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            payload.as_union_value(),
        )
    }

    pub fn encode_entity_snapshot_with_blocking(
        server_tick: u32,
        entities: &[EntityStateWire],
        vitals: PlayerVitalsWire,
        blocking_players: &[u64],
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(entities.len() * 40 + 128);
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
        let blocking = builder.create_vector(blocking_players);
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                hunger: vitals.hunger,
                max_hunger: vitals.max_hunger,
                level: vitals.level,
                experience: vitals.experience,
                experience_to_next: vitals.experience_to_next,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
                blocking: vitals.blocking,
            },
        );
        let mut table = fb::EntitySnapshotBuilder::new(&mut builder);
        table.add_server_tick(server_tick);
        table.add_entities(entities);
        table.add_self_vitals(self_vitals);
        table.add_blocking_players(blocking);
        let payload = table.finish();
        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            payload.as_union_value(),
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
        let durabilities: Vec<_> = drops
            .iter()
            .filter(|drop| drop.max_durability != 0)
            .map(|drop| ItemDropDurabilityWire {
                entity_id: drop.entity_id,
                durability: drop.durability,
                max_durability: drop.max_durability,
            })
            .collect();
        encode_entity_snapshot_with_explicit_durabilities(
            server_tick,
            entities,
            drops,
            mobs,
            vitals,
            structures,
            &durabilities,
            0,
            &[],
        )
    }

    /// Encodes the V20 party projection independently so malformed decoder boundaries
    /// do not require a simulation capable of producing them.
    pub fn encode_entity_snapshot_with_party(
        party_leader_entity_id: u64,
        party_members: &[PartyMemberStateWire],
    ) -> Vec<u8> {
        if party_leader_entity_id == 0 {
            return encode_entity_snapshot_with_explicit_durabilities(
                1,
                &[],
                &[],
                &[],
                PlayerVitalsWire::default(),
                &[],
                &[],
                party_leader_entity_id,
                party_members,
            );
        }
        let mut roster = vec![PartyRosterMemberWire {
            character_id: 1,
            entity_id: party_leader_entity_id,
            name: Some("Leader"),
            online: true,
        }];
        roster.extend(party_members.iter().enumerate().map(|(index, member)| {
            PartyRosterMemberWire {
                character_id: index as u64 + 2,
                entity_id: member.entity_id,
                name: Some("Member"),
                online: member.entity_id != 0,
            }
        }));
        encode_entity_snapshot_with_roster(party_leader_entity_id, party_members, &roster, &[])
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_entity_snapshot_with_explicit_durabilities(
        server_tick: u32,
        entities: &[EntityStateWire],
        drops: &[ItemDropStateWire],
        mobs: &[MobStateWire],
        vitals: PlayerVitalsWire,
        structures: &[StructureStateWire],
        durabilities: &[ItemDropDurabilityWire],
        party_leader_entity_id: u64,
        party_members: &[PartyMemberStateWire],
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(
            entities.len() * 40
                + drops.len() * 24
                + durabilities.len() * 16
                + mobs.len() * 64
                + structures.len() * 48
                + party_members.len() * 32
                + 128,
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
        let laid_out: Vec<fb::ItemDropDurability> = durabilities
            .iter()
            .map(|state| {
                fb::ItemDropDurability::new(state.entity_id, state.durability, state.max_durability)
            })
            .collect();
        let drop_durabilities = builder.create_vector(&laid_out);

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
                table.add_target_entity_id(mob.target_entity_id);
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
                table.add_lit(structure.lit);
                table.finish()
            })
            .collect();
        let structures = builder.create_vector(&laid_out);

        let laid_out: Vec<fb::PartyMemberState> = party_members
            .iter()
            .map(|member| {
                fb::PartyMemberState::new(
                    member.entity_id,
                    &fb::Vec3::new(member.pos[0], member.pos[1], member.pos[2]),
                    member.health,
                    member.max_health,
                    member.alive,
                )
            })
            .collect();
        let party_members = builder.create_vector(&laid_out);
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                hunger: vitals.hunger,
                max_hunger: vitals.max_hunger,
                level: vitals.level,
                experience: vitals.experience,
                experience_to_next: vitals.experience_to_next,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
                blocking: false,
            },
        );

        let mut table = fb::EntitySnapshotBuilder::new(&mut builder);
        table.add_server_tick(server_tick);
        table.add_entities(entities);
        table.add_drops(drops);
        table.add_mobs(mobs);
        table.add_self_vitals(self_vitals);
        table.add_structures(structures);
        table.add_party_leader_entity_id(party_leader_entity_id);
        if !laid_out.is_empty() {
            table.add_party_members(party_members);
        }
        if !durabilities.is_empty() {
            table.add_drop_durabilities(drop_durabilities);
        }
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
                hunger: vitals.hunger,
                max_hunger: vitals.max_hunger,
                level: vitals.level,
                experience: vitals.experience,
                experience_to_next: vitals.experience_to_next,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
                blocking: false,
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
                dead_players: None,
                drop_durabilities: None,
                party_leader_entity_id: 0,
                party_members: None,
                ..Default::default()
            },
        );

        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            snapshot.as_union_value(),
        )
    }

    /// Encodes a snapshot carrying nothing but its process tick and both world-clock
    /// projections.
    ///
    /// Every vector empty, deliberately: what its callers are checking is the clock pair's
    /// journey across the wire, and an entity in the frame would only be another thing
    /// that could go wrong.
    pub fn encode_entity_snapshot_at_world_tick(
        server_tick: u32,
        tick_of_day: u32,
        world_tick: u64,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let vitals = PlayerVitalsWire::default();
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                hunger: vitals.hunger,
                max_hunger: vitals.max_hunger,
                level: vitals.level,
                experience: vitals.experience,
                experience_to_next: vitals.experience_to_next,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
                blocking: false,
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
                world_tick,
                dead_players: None,
                drop_durabilities: None,
                party_leader_entity_id: 0,
                party_members: None,
                ..Default::default()
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
    pub fn encode_player_appearance(
        entity_id: u64,
        appearance: Option<AppearanceWire>,
        name: Option<&str>,
        level: u16,
    ) -> Vec<u8> {
        encode_player_appearance_with_worn(entity_id, appearance, name, level, [0; 4])
    }

    /// A `PlayerAppearance` with explicit worn item ids, including zero for an empty
    /// location. Kept beside the ordinary helper so most tests keep the empty default.
    pub fn encode_player_appearance_with_worn(
        entity_id: u64,
        appearance: Option<AppearanceWire>,
        name: Option<&str>,
        level: u16,
        worn: [u16; 4],
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let appearance = appearance.map(|a| appearance_offset(&mut builder, a));
        let name = name.map(|name| builder.create_string(name));
        let payload = fb::PlayerAppearance::create(
            &mut builder,
            &fb::PlayerAppearanceArgs {
                entity_id,
                appearance,
                name,
                level,
                worn_head: worn[0],
                worn_chest: worn[1],
                worn_legs: worn[2],
                worn_offhand: worn[3],
            },
        );
        finish_envelope(
            builder,
            fb::Payload::PlayerAppearance,
            payload.as_union_value(),
        )
    }

    /// A pre-V17 `PlayerAppearance`, whose builder never names `level` at all.
    pub fn encode_player_appearance_without_level(
        entity_id: u64,
        appearance: Option<AppearanceWire>,
        name: Option<&str>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let appearance = appearance.map(|a| appearance_offset(&mut builder, a));
        let name = name.map(|name| builder.create_string(name));
        let payload = fb::PlayerAppearance::create(
            &mut builder,
            &fb::PlayerAppearanceArgs {
                entity_id,
                appearance,
                name,
                ..Default::default()
            },
        );
        finish_envelope(
            builder,
            fb::Payload::PlayerAppearance,
            payload.as_union_value(),
        )
    }

    /// A server-owned leave countdown, including zero for the decoder's malformed
    /// boundary test.
    pub fn encode_leave_started(remaining_ms: u32) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload =
            fb::LeaveStarted::create(&mut builder, &fb::LeaveStartedArgs { remaining_ms });
        finish_envelope(builder, fb::Payload::LeaveStarted, payload.as_union_value())
    }

    /// One authoritative answer to a leave cancellation request, including malformed
    /// combinations for the decoder boundary tests.
    pub fn encode_leave_cancel_result(accepted: bool, remaining_ms: u32) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload = fb::LeaveCancelResult::create(
            &mut builder,
            &fb::LeaveCancelResultArgs {
                accepted,
                remaining_ms,
            },
        );
        finish_envelope(
            builder,
            fb::Payload::LeaveCancelResult,
            payload.as_union_value(),
        )
    }

    pub fn encode_chat_message(
        sender_entity_id: u64,
        sender_name: Option<&str>,
        text: Option<&str>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let sender_name = sender_name.map(|value| builder.create_string(value));
        let text = text.map(|value| builder.create_string(value));
        let payload = fb::ChatMessage::create(
            &mut builder,
            &fb::ChatMessageArgs {
                sender_entity_id,
                sender_name,
                text,
            },
        );
        finish_envelope(builder, fb::Payload::ChatMessage, payload.as_union_value())
    }

    pub fn encode_party_invite(
        from_entity_id: u64,
        from_name: Option<&str>,
        expires_ms: u32,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let from_name = from_name.map(|value| builder.create_string(value));
        let payload = fb::PartyInvite::create(
            &mut builder,
            &fb::PartyInviteArgs {
                from_entity_id,
                from_name,
                expires_ms,
            },
        );
        finish_envelope(builder, fb::Payload::PartyInvite, payload.as_union_value())
    }

    pub fn encode_mob_hit(attacker_entity_id: u64, pos: Option<[f32; 3]>) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let pos = pos.map(|pos| fb::Vec3::new(pos[0], pos[1], pos[2]));
        let payload = fb::MobHit::create(
            &mut builder,
            &fb::MobHitArgs {
                attacker_entity_id,
                attacker_pos: pos.as_ref(),
            },
        );
        finish_envelope(builder, fb::Payload::MobHit, payload.as_union_value())
    }

    pub fn encode_loot_state(
        corpse_id: u64,
        revision: u32,
        entries: Option<&[LootEntry]>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let entries = entries.map(|entries| {
            let laid_out: Vec<_> = entries
                .iter()
                .map(|entry| {
                    fb::LootEntry::new(
                        entry.entry_id,
                        entry.item_id,
                        entry.count,
                        entry.durability,
                        entry.max_durability,
                    )
                })
                .collect();
            builder.create_vector(&laid_out)
        });
        let payload = fb::LootState::create(
            &mut builder,
            &fb::LootStateArgs {
                corpse_id,
                revision,
                entries,
                silver: 0,
            },
        );
        finish_envelope(builder, fb::Payload::LootState, payload.as_union_value())
    }

    pub fn encode_empty_loot_with_silver(silver: u32) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::new();
        let entries = builder.create_vector::<fb::LootEntry>(&[]);
        let loot = fb::LootState::create(
            &mut builder,
            &fb::LootStateArgs {
                corpse_id: 7,
                revision: 1,
                entries: Some(entries),
                silver,
            },
        );
        finish_envelope(builder, fb::Payload::LootState, loot.as_union_value())
    }

    /// Builds one `MapTile` frame from raw parts, so a test can present the decoder
    /// with a tile no correct server would send: a short array, an absent one, a scale
    /// this contract has no member for, a surface byte nobody names.
    pub fn encode_map_tile(
        origin_x: i32,
        origin_z: i32,
        scale: u8,
        height: Option<&[u8]>,
        surface: Option<&[u8]>,
        explored: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let height = height.map(|bytes| builder.create_vector(bytes));
        let surface = surface.map(|bytes| builder.create_vector(bytes));
        let explored = explored.map(|bytes| builder.create_vector(bytes));
        let payload = fb::MapTile::create(
            &mut builder,
            &fb::MapTileArgs {
                origin_x,
                origin_z,
                scale,
                height,
                surface,
                explored,
            },
        );
        finish_envelope(builder, fb::Payload::MapTile, payload.as_union_value())
    }

    pub fn encode_map_explored(columns: Option<&[(i32, i32)]>) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let columns = columns.map(|columns| {
            let laid_out: Vec<_> = columns
                .iter()
                .map(|(cx, cz)| fb::MapColumn::new(*cx, *cz))
                .collect();
            builder.create_vector(&laid_out)
        });
        let payload = fb::MapExplored::create(&mut builder, &fb::MapExploredArgs { columns });
        finish_envelope(builder, fb::Payload::MapExplored, payload.as_union_value())
    }

    /// One mark as raw wire parts, so a test can build a list a correct server never
    /// sends: a reserved id, an `Unknown` kind, a note past the bound.
    pub struct MarkerWire<'a> {
        pub marker_id: u64,
        pub x: i32,
        pub z: i32,
        pub kind: u8,
        pub note: Option<&'a str>,
    }

    pub fn encode_marker_list(markers: Option<&[MarkerWire<'_>]>) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let markers = markers.map(|markers| {
            // Every note is written before any marker table opens: a table may not be
            // under construction while a string is created.
            let notes: Vec<_> = markers
                .iter()
                .map(|marker| marker.note.map(|note| builder.create_string(note)))
                .collect();
            let laid_out: Vec<_> = markers
                .iter()
                .zip(notes)
                .map(|(marker, note)| {
                    fb::Marker::create(
                        &mut builder,
                        &fb::MarkerArgs {
                            marker_id: marker.marker_id,
                            x: marker.x,
                            z: marker.z,
                            kind: fb::MarkerKind(marker.kind),
                            note,
                        },
                    )
                })
                .collect();
            builder.create_vector(&laid_out)
        });
        let payload = fb::MarkerList::create(&mut builder, &fb::MarkerListArgs { markers });
        finish_envelope(builder, fb::Payload::MarkerList, payload.as_union_value())
    }

    pub fn encode_resident_appearance(
        entity_id: u64,
        name: Option<&str>,
        role: u8,
        appearance: Option<AppearanceWire>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let appearance = appearance.map(|a| appearance_offset(&mut builder, a));
        let name = name.map(|name| builder.create_string(name));
        let payload = fb::ResidentAppearance::create(
            &mut builder,
            &fb::ResidentAppearanceArgs {
                entity_id,
                name,
                role: fb::ResidentRole(role),
                appearance,
            },
        );
        finish_envelope(
            builder,
            fb::Payload::ResidentAppearance,
            payload.as_union_value(),
        )
    }

    /// A `VendorState` whose two vectors can each be absent, so a test can build the one
    /// message shape this contract does not have.
    pub fn encode_vendor_state(
        entity_id: u64,
        revision: u32,
        sells: Option<&[(u16, u16)]>,
        buys: Option<&[(u16, u16)]>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let lay_out = |builder: &mut FlatBufferBuilder<'static>, entries: &[(u16, u16)]| {
            let laid_out: Vec<_> = entries
                .iter()
                .map(|(item_id, price)| fb::VendorEntry::new(*item_id, *price))
                .collect();
            builder.create_vector(&laid_out)
        };
        let sells = sells.map(|entries| lay_out(&mut builder, entries));
        let buys = buys.map(|entries| lay_out(&mut builder, entries));
        let payload = fb::VendorState::create(
            &mut builder,
            &fb::VendorStateArgs {
                entity_id,
                revision,
                sells,
                buys,
            },
        );
        finish_envelope(builder, fb::Payload::VendorState, payload.as_union_value())
    }

    pub fn encode_vendor_closed(entity_id: u64) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload = fb::VendorClosed::create(&mut builder, &fb::VendorClosedArgs { entity_id });
        finish_envelope(builder, fb::Payload::VendorClosed, payload.as_union_value())
    }

    /// A state builder that can omit each required field.
    pub fn encode_player_trade_state(
        partner_entity_id: u64,
        partner_name: Option<&str>,
        revision: u32,
        my_offer: Option<&[PlayerTradeSlotWire]>,
        their_offer: Option<&[PlayerTradeSlotWire]>,
        silver: (u32, u32),
        confirmed: (bool, bool),
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY * 2);
        let lay_out = |builder: &mut FlatBufferBuilder<'static>, slots: &[PlayerTradeSlotWire]| {
            let laid_out: Vec<_> = slots
                .iter()
                .map(|slot| {
                    fb::PlayerTradeSlot::new(
                        slot.trade_slot,
                        slot.pack_slot,
                        slot.item_id,
                        slot.count,
                        slot.durability,
                        slot.max_durability,
                    )
                })
                .collect();
            builder.create_vector(&laid_out)
        };
        let partner_name = partner_name.map(|name| builder.create_string(name));
        let my_offer = my_offer.map(|slots| lay_out(&mut builder, slots));
        let their_offer = their_offer.map(|slots| lay_out(&mut builder, slots));
        let payload = fb::PlayerTradeState::create(
            &mut builder,
            &fb::PlayerTradeStateArgs {
                partner_entity_id,
                partner_name,
                revision,
                my_offer,
                their_offer,
                my_silver: silver.0,
                their_silver: silver.1,
                my_confirmed: confirmed.0,
                their_confirmed: confirmed.1,
            },
        );
        finish_envelope(
            builder,
            fb::Payload::PlayerTradeState,
            payload.as_union_value(),
        )
    }

    pub fn encode_player_trade_closed(partner_entity_id: u64, reason: u8) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload = fb::PlayerTradeClosed::create(
            &mut builder,
            &fb::PlayerTradeClosedArgs {
                partner_entity_id,
                reason: fb::PlayerTradeCloseReason(reason),
            },
        );
        finish_envelope(
            builder,
            fb::Payload::PlayerTradeClosed,
            payload.as_union_value(),
        )
    }

    /// A snapshot carrying the recipient's vitals, optionally the weather where they
    /// stand, and optionally one structure whose `lit` byte is never written.
    ///
    /// Both options exist to reach a frame the ordinary builders cannot. `None` weather
    /// omits the struct field entirely, which is the server that keeps none, and `Some`
    /// writes it kind byte and all, so a test can name a kind this contract has no member
    /// for. The structure is written through a builder that never calls `add_lit`, which
    /// is the pre-V26 frame — `StructureStateWire` writes that field unconditionally, so
    /// "wrote `true`" and "wrote nothing" stay two distinguishable frames.
    pub fn encode_entity_snapshot_with_weather_and_bare_structure(
        weather: Option<(u8, u8)>,
        structure: Option<StructureStateWire>,
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let vitals = PlayerVitalsWire::default();
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                hunger: vitals.hunger,
                max_hunger: vitals.max_hunger,
                level: vitals.level,
                experience: vitals.experience,
                experience_to_next: vitals.experience_to_next,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
                blocking: vitals.blocking,
            },
        );
        let structures = structure.map(|structure| {
            let laid_out = {
                let mut table = fb::StructureStateBuilder::new(&mut builder);
                table.add_structure_id(structure.structure_id);
                table.add_kind(structure.kind);
                if let Some([x, y, z]) = structure.anchor {
                    table.add_anchor(&fb::BlockCoord::new(x, y, z));
                }
                table.add_facing(structure.facing);
                table.add_owner_entity_id(structure.owner_entity_id);
                table.finish()
            };
            builder.create_vector(&[laid_out])
        });
        let mut table = fb::EntitySnapshotBuilder::new(&mut builder);
        table.add_server_tick(1);
        table.add_self_vitals(self_vitals);
        if let Some(structures) = structures {
            table.add_structures(structures);
        }
        if let Some((kind, intensity)) = weather {
            table.add_weather(&fb::WeatherState::new(fb::WeatherKind(kind), intensity));
        }
        let payload = table.finish();
        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            payload.as_union_value(),
        )
    }

    /// A `StormWarning` built from raw wire parts, so a test can name a phase this
    /// contract has no member for and a countdown a correct server never pairs with it.
    pub fn encode_storm_warning(seconds_until: u32, phase: u8) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload = fb::StormWarning::create(
            &mut builder,
            &fb::StormWarningArgs {
                seconds_until,
                phase: fb::StormPhase(phase),
            },
        );
        finish_envelope(builder, fb::Payload::StormWarning, payload.as_union_value())
    }

    /// One warded column as raw wire parts, so a test can build a set a correct server
    /// never sends: an `Unknown` kind, or one column named twice.
    #[derive(Debug, Clone, Copy)]
    pub struct WardedColumnWire {
        pub cx: i32,
        pub cz: i32,
        pub kind: u8,
        pub mine: bool,
    }

    /// A `WardsNearby` whose vector can be absent, present-and-empty, or anything else.
    ///
    /// The first two are the same message by contract, and this helper keeps them
    /// distinguishable on the wire so that a test can say so rather than assume it.
    pub fn encode_wards_nearby(columns: Option<&[WardedColumnWire]>) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(
            columns.map_or(0, |columns| columns.len() * 16) + super::BUILDER_CAPACITY,
        );
        let columns = columns.map(|columns| {
            let laid_out: Vec<_> = columns
                .iter()
                .map(|column| {
                    fb::WardedColumn::new(
                        column.cx,
                        column.cz,
                        fb::WardKind(column.kind),
                        column.mine,
                    )
                })
                .collect();
            builder.create_vector(&laid_out)
        });
        let payload = fb::WardsNearby::create(&mut builder, &fb::WardsNearbyArgs { columns });
        finish_envelope(builder, fb::Payload::WardsNearby, payload.as_union_value())
    }

    /// Encodes a `VoiceHeard` envelope. `None` omits the vector entirely, which is how
    /// an absent field reaches the decoder; an empty slice is the other shape of no
    /// audio, and the decoder owes both the same answer.
    pub fn encode_voice_heard(
        speaker_entity_id: u64,
        sequence: u32,
        opus: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut builder =
            FlatBufferBuilder::with_capacity(opus.map_or(0, <[u8]>::len) + super::BUILDER_CAPACITY);
        let opus = opus.map(|opus| builder.create_vector(opus));
        let payload = fb::VoiceHeard::create(
            &mut builder,
            &fb::VoiceHeardArgs {
                speaker_entity_id,
                sequence,
                opus,
            },
        );
        finish_envelope(builder, fb::Payload::VoiceHeard, payload.as_union_value())
    }

    pub fn encode_loot_closed(corpse_id: u64) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);
        let payload = fb::LootClosed::create(&mut builder, &fb::LootClosedArgs { corpse_id });
        finish_envelope(builder, fb::Payload::LootClosed, payload.as_union_value())
    }

    pub fn encode_entity_snapshot_with_roster(
        party_leader_entity_id: u64,
        party_members: &[PartyMemberStateWire],
        roster: &[PartyRosterMemberWire<'_>],
        accessible_corpses: &[u64],
    ) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::with_capacity(super::BUILDER_CAPACITY);

        let laid_out: Vec<fb::PartyMemberState> = party_members
            .iter()
            .map(|member| {
                fb::PartyMemberState::new(
                    member.entity_id,
                    &fb::Vec3::new(member.pos[0], member.pos[1], member.pos[2]),
                    member.health,
                    member.max_health,
                    member.alive,
                )
            })
            .collect();
        let party_members = builder.create_vector(&laid_out);

        let roster_offsets: Vec<_> = roster
            .iter()
            .map(|member| {
                let name = member.name.map(|name| builder.create_string(name));
                fb::PartyRosterMember::create(
                    &mut builder,
                    &fb::PartyRosterMemberArgs {
                        character_id: member.character_id,
                        entity_id: member.entity_id,
                        name,
                        online: member.online,
                    },
                )
            })
            .collect();
        let roster = builder.create_vector(&roster_offsets);
        let accessible = builder.create_vector(accessible_corpses);
        let mob_offsets: Vec<_> = accessible_corpses
            .iter()
            .map(|corpse_id| {
                let mut mob = fb::MobStateBuilder::new(&mut builder);
                mob.add_entity_id(*corpse_id);
                mob.add_kind(fb::MobKind::Draugr);
                mob.add_pos(&fb::Vec3::new(0.0, 0.0, 0.0));
                mob.add_vel(&fb::Vec3::new(0.0, 0.0, 0.0));
                mob.add_max_health(60);
                mob.add_action(fb::MobAction::Corpse);
                mob.finish()
            })
            .collect();
        let mobs = builder.create_vector(&mob_offsets);
        let vitals = PlayerVitalsWire::default();
        let self_vitals = fb::PlayerVitals::create(
            &mut builder,
            &fb::PlayerVitalsArgs {
                health: vitals.health,
                max_health: vitals.max_health,
                hunger: vitals.hunger,
                max_hunger: vitals.max_hunger,
                level: vitals.level,
                experience: vitals.experience,
                experience_to_next: vitals.experience_to_next,
                life_state: vitals.life_state,
                respawn_ticks: vitals.respawn_ticks,
                invulnerable: vitals.invulnerable,
                blocking: false,
            },
        );
        let mut table = fb::EntitySnapshotBuilder::new(&mut builder);
        table.add_server_tick(1);
        table.add_self_vitals(self_vitals);
        if !mob_offsets.is_empty() {
            table.add_mobs(mobs);
        }
        table.add_party_leader_entity_id(party_leader_entity_id);
        if !laid_out.is_empty() {
            table.add_party_members(party_members);
        }
        if !roster_offsets.is_empty() {
            table.add_party_roster(roster);
        }
        if !accessible_corpses.is_empty() {
            table.add_accessible_loot_corpses(accessible);
        }
        let payload = table.finish();
        finish_envelope(
            builder,
            fb::Payload::EntitySnapshot,
            payload.as_union_value(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::server_side;
    use super::server_side::{
        AppearanceWire, CharacterSummaryWire, DEFAULT_TOKEN, EntityStateWire,
        ItemDropDurabilityWire, ItemDropStateWire, MarkerWire, MobStateWire, PartyMemberStateWire,
        PartyRosterMemberWire, PlayerTradeSlotWire, PlayerVitalsWire, ProjectileStateWire,
        StructureStateWire, WardedColumnWire, WelcomeWire, encode_action_refused,
        encode_bare_block_update, encode_bare_chunk_data, encode_bare_chunk_unload,
        encode_bare_entity_snapshot, encode_block_update, encode_chat_message, encode_chunk_data,
        encode_chunk_unload, encode_empty_inventory_with_silver, encode_empty_loot_with_silver,
        encode_entity_snapshot, encode_entity_snapshot_with, encode_entity_snapshot_with_dead,
        encode_entity_snapshot_with_drop_durabilities, encode_entity_snapshot_with_drops,
        encode_entity_snapshot_with_party, encode_entity_snapshot_with_projectiles,
        encode_entity_snapshot_with_roster, encode_entity_snapshot_with_weather_and_bare_structure,
        encode_entity_snapshot_without_vitals, encode_inventory_state,
        encode_inventory_state_with_durability, encode_learned_mounts, encode_leave_cancel_result,
        encode_leave_started, encode_loot_closed, encode_loot_state, encode_map_explored,
        encode_map_tile, encode_marker_list, encode_mine_progress, encode_mob_hit,
        encode_party_invite, encode_player_appearance, encode_player_appearance_with_worn,
        encode_player_appearance_without_level, encode_player_trade_closed,
        encode_player_trade_state, encode_resident_appearance, encode_server_character_list,
        encode_server_reject, encode_server_welcome, encode_snapshot_with_mounts,
        encode_storm_warning, encode_vendor_closed, encode_vendor_state, encode_wards_nearby,
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

    fn trade_state(
        my_offer: Option<&[PlayerTradeSlotWire]>,
        their_offer: Option<&[PlayerTradeSlotWire]>,
    ) -> Vec<u8> {
        encode_player_trade_state(
            77,
            Some("Eydis"),
            1,
            my_offer,
            their_offer,
            (0, 0),
            (false, false),
        )
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
    ///
    /// **V8 adds one tag and moves the version, and there the deciding fact is
    /// direction.** "A peer drops a tag it cannot name" is a property of *this* decoder,
    /// which is a client's — the sweep below is what makes it one. A server does not drop,
    /// so tag 25 travelling client→server means a V7 server and a V8 client would handshake
    /// cleanly and die on the first stack anybody put down. Every client→server tag arrived
    /// with a bump; the one appended without one goes the other way.
    ///
    /// **V9 adds no tag at all and moves the version anyway**, which is what makes the
    /// preceding paragraph too narrow to be the rule. It appends `MobAction::Dying`, an
    /// enum member inside a table field, travelling server→client — the direction tag 20
    /// got away with. The exemption does not follow it, because the exemption belongs to
    /// the *union switch*: an unnamed tag is a whole frame this decoder never opens, while
    /// [`malformed_mobs_are_protocol_errors`] is what an unnamed enum member gets instead —
    /// its "an action a newer contract added" case is precisely this failure.
    /// A V8 client against a V9 server would therefore connect perfectly and drop the
    /// session on the first creature anybody killed.
    ///
    /// **V10 adds no tag either, and moves the version for a reason none of the four
    /// paragraphs above reaches.** It appends a *table field* — `EntitySnapshot.dead_players`
    /// — and an unknown table field really is dropped: a V9 peer never looks the id up, so no
    /// argument about *this* decoder decides it. The argument runs the other way for the
    /// first time. The field's invariant ties it to `self_vitals`, [`Handshake`] enforces it,
    /// and a V9 server never sends the vector — so a client built against this contract would
    /// connect perfectly, play perfectly, and end the session the first time it died.
    ///
    /// **V11 appends another table field, and moves for the silent direction.** A missing
    /// `drop_durabilities` vector says every visible drop is wearless, so this decoder would
    /// accept a worn drop from a V10 server as pristine while the authoritative server still
    /// returned it worn on collection. The peers would silently disagree about one entity.
    ///
    /// **V12 appends the leaving exchange and V13 appends `PlayerAppearance.name`.** The
    /// former adds a client request an older server would reject; the latter is a required
    /// string this decoder refuses when absent. Both would fail only after a clean handshake
    /// without their bumps.
    ///
    /// **V14 appends `MobKind::Deer` and `MobAction::Flee`.** Both are enum members in a
    /// `MobState`; this decoder refuses either unknown member, so an older client would
    /// otherwise connect and then end its session when a deer entered view.
    ///
    /// **V15 appends `ConsumeRequest` and hunger to `PlayerVitals`.** The request is a
    /// union member an older server refuses, and the new maximum is a decoder invariant:
    /// its absent-field zero is not a usable V15 snapshot.
    ///
    /// **V16 appends `RecipeID::CookedMeat`.** It travels client to server in a
    /// `CraftRequest`, so a V15 server would reject it only after a clean handshake.
    ///
    /// **V17 appends progression to `PlayerVitals` and `PlayerAppearance`.** This
    /// decoder requires a non-zero experience denominator, which a V16 server never
    /// sends; without the bump it would refuse the first snapshot after a clean
    /// handshake.
    ///
    /// **V18 appends equipment layout metadata and worn item ids.** This decoder
    /// requires a non-zero equipment count that a V17 server never sends, so the
    /// mismatch must be caught at admission.
    ///
    /// **V19 appends six wearable `RecipeID` members.** They travel client to server in
    /// a `CraftRequest`, so a V18 server would reject them only after a clean handshake.
    ///
    /// **V20 appends chat and party requests plus party state in snapshots.** A V19
    /// server cannot name either request and would otherwise fail only on first use.
    ///
    /// **V21 appends two corpse-loot requests and two authoritative answers.** A V20
    /// server cannot name either request; stable roster and corpse access also append to
    /// snapshots so offline party state cannot be silently discarded.
    ///
    /// **V22 appends `BlockRequest` and raised-shield consistency state.** A V21 server
    /// cannot name the client request, while a V22 client cannot accept snapshots that
    /// omit the matching blocking statements after a clean handshake.
    ///
    /// **V23 appends `LootTakeAllRequest`.** A V22 server cannot name that tag and closes
    /// the session rather than dropping it, so the mismatch has to be caught at admission
    /// rather than on the first corpse a player empties. Nothing arrives server to client
    /// for it: the answer is the `LootState`, `LootClosed` and `TakeLoot`/`InventoryFull`
    /// refusal V21 already defined.
    ///
    /// **V24 appends the map: six members, three of them client to server.** A V23 server
    /// cannot name `MapTileRequest`, `MarkerPlaceRequest` or `MarkerRemoveRequest` and
    /// closes the session rather than dropping any of them, so each owes the bump alone
    /// and the three are taken together. The three that travel back would have owed
    /// nothing. Both refusal enums also gain members and neither independently owes a
    /// bump: `RefusedAction::from_wire` and `RefusalReason::from_wire` are total by
    /// design, so an unreadable member costs one sentence rather than the session.
    ///
    /// **V25 appends the settlement: five members, two of them client to server.** A V24
    /// server cannot name `NpcInteractRequest` or `TradeRequest` and closes the session on
    /// either, so each owes the bump alone. What makes this version worth reading twice is
    /// the member that is *not* in the union: `MobKind::Villager` travels server to client
    /// inside a `MobState`, and it moves the version where an appended `RefusalReason`
    /// member does not — because [`MobKind::from_wire`] answers `None` and the caller ends
    /// the session, while `RefusalReason::from_wire` is total and answers `Unknown`. Two
    /// appended enum members, opposite conclusions, and the receiver is the whole of the
    /// difference.
    ///
    /// **V26 appends the Fimbulvetr: two union members, neither of which is the break.**
    /// `StormWarning` and `WardsNearby` both travel server to client, and an older client
    /// drops a tag it cannot name — a warning lost, some shading lost, and the session
    /// intact. What moved the version is not in this union at all:
    /// `StructureKind::Runestone` is an enum member inside a table field whose invariant
    /// is a known non-zero kind, so [`StructureKind::from_wire`] answers `None` and the
    /// caller ends the session. Same shape as `MobKind::Villager` one version earlier, and
    /// the same conclusion.
    ///
    /// **V27 batches leave cancellation and the stable contract.** `LeaveCancelRequest`,
    /// `MountRequest` and `DismountRequest` travel client to server, so a V26 server would
    /// close the session on any unknown tag. Their server-to-client companions are safely
    /// droppable alone; every request independently owes the bump.
    ///
    /// **V29 appends the absolute world clock.** An absent scalar decodes as zero, which
    /// is a plausible fresh world rather than an error, so a V28 server would silently
    /// restart multi-day presentation on every connection. That semantic mismatch owes
    /// the bump even though FlatBuffers accepts the older table.
    ///
    /// The rule that generalises, now that nine shapes have been argued: **ask what the
    /// receiver does with the value it does not recognise, not which way it travelled.**
    /// Dropping it is a bump avoided; refusing it is a bump owed. The same words are in
    /// `schemas/common.fbs`, `schemas/AGENTS.md` and the Go half of this pin.
    #[test]
    fn protocol_v30_adds_the_authoritative_voice_relay() {
        assert_eq!(fb::ProtocolVersion::Unknown.0, 0);
        assert_eq!(fb::ProtocolVersion::Current.0, 30);
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
            (fb::Payload::DropItemRequest, 25),
            (fb::Payload::LeaveRequest, 26),
            (fb::Payload::LeaveStarted, 27),
            (fb::Payload::ConsumeRequest, 28),
            (fb::Payload::ChatRequest, 29),
            (fb::Payload::ChatMessage, 30),
            (fb::Payload::PartyRequest, 31),
            (fb::Payload::PartyInvite, 32),
            (fb::Payload::LootOpenRequest, 33),
            (fb::Payload::LootTakeRequest, 34),
            (fb::Payload::LootState, 35),
            (fb::Payload::LootClosed, 36),
            (fb::Payload::MobHit, 37),
            (fb::Payload::BlockRequest, 38),
            (fb::Payload::LootTakeAllRequest, 39),
            (fb::Payload::MapTileRequest, 40),
            (fb::Payload::MapTile, 41),
            (fb::Payload::MapExplored, 42),
            (fb::Payload::MarkerPlaceRequest, 43),
            (fb::Payload::MarkerRemoveRequest, 44),
            (fb::Payload::MarkerList, 45),
            (fb::Payload::ResidentAppearance, 46),
            (fb::Payload::NpcInteractRequest, 47),
            (fb::Payload::VendorState, 48),
            (fb::Payload::TradeRequest, 49),
            (fb::Payload::VendorClosed, 50),
            (fb::Payload::StormWarning, 51),
            (fb::Payload::WardsNearby, 52),
            (fb::Payload::LeaveCancelRequest, 53),
            (fb::Payload::LeaveCancelResult, 54),
            (fb::Payload::LearnedMounts, 55),
            (fb::Payload::MountRequest, 56),
            (fb::Payload::DismountRequest, 57),
            (fb::Payload::PlayerTradeRequest, 58),
            (fb::Payload::PlayerTradeState, 59),
            (fb::Payload::PlayerTradeClosed, 60),
            (fb::Payload::VoiceFrame, 61),
            (fb::Payload::VoiceHeard, 62),
        ] {
            assert_eq!(tag.0, value);
        }

        // Membership, not just ordering. A swing is still answered by the next snapshot
        // and nothing else, and so is a craft and a repair; a *refused* placement is now
        // answered by `ActionRefused`, and an accepted one is not. V12's `LeaveStarted`
        // is the deliberate exception: an acknowledgement carrying the server's timer,
        // never a client-owned outcome. The size of the union is the only place that
        // membership can be checked. V8's one does not break that run: a
        // drop is answered by the complete `InventoryState` that follows and by the
        // `ItemDropState` in the next snapshot, both of which already existed. The extra
        // member is `NONE`, the implicit zero every FlatBuffers union carries.
        assert_eq!(
            fb::Payload::ENUM_VALUES.len(),
            63,
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
    const CLASSIFICATION: [(fb::Payload, Handling); 63] = [
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
        (fb::Payload::DropItemRequest, Handling::ClientOnly),
        (fb::Payload::LeaveRequest, Handling::ClientOnly),
        (fb::Payload::LeaveStarted, Handling::Consumed),
        (fb::Payload::ConsumeRequest, Handling::ClientOnly),
        (fb::Payload::ChatRequest, Handling::ClientOnly),
        (fb::Payload::ChatMessage, Handling::Consumed),
        (fb::Payload::PartyRequest, Handling::ClientOnly),
        (fb::Payload::PartyInvite, Handling::Consumed),
        (fb::Payload::LootOpenRequest, Handling::ClientOnly),
        (fb::Payload::LootTakeRequest, Handling::ClientOnly),
        (fb::Payload::LootState, Handling::Consumed),
        (fb::Payload::LootClosed, Handling::Consumed),
        (fb::Payload::MobHit, Handling::Consumed),
        (fb::Payload::BlockRequest, Handling::ClientOnly),
        (fb::Payload::LootTakeAllRequest, Handling::ClientOnly),
        (fb::Payload::MapTileRequest, Handling::ClientOnly),
        (fb::Payload::MapTile, Handling::Consumed),
        (fb::Payload::MapExplored, Handling::Consumed),
        (fb::Payload::MarkerPlaceRequest, Handling::ClientOnly),
        (fb::Payload::MarkerRemoveRequest, Handling::ClientOnly),
        (fb::Payload::MarkerList, Handling::Consumed),
        (fb::Payload::ResidentAppearance, Handling::Consumed),
        (fb::Payload::NpcInteractRequest, Handling::ClientOnly),
        (fb::Payload::VendorState, Handling::Consumed),
        (fb::Payload::TradeRequest, Handling::ClientOnly),
        (fb::Payload::VendorClosed, Handling::Consumed),
        // V26's two, both server→client and both read by an arm of their own since the
        // payload half of #463. They were `Deferred` through the two parts before it —
        // the staged shape V24's map payloads and V25's stall each had — and `Deferred`
        // meant "this build has no arm yet" rather than "this contract has no member".
        (fb::Payload::StormWarning, Handling::Consumed),
        (fb::Payload::WardsNearby, Handling::Consumed),
        // V27's request stays intent-only; its result is fully validated and consumed.
        (fb::Payload::LeaveCancelRequest, Handling::ClientOnly),
        (fb::Payload::LeaveCancelResult, Handling::Consumed),
        (fb::Payload::LearnedMounts, Handling::Consumed),
        (fb::Payload::MountRequest, Handling::ClientOnly),
        (fb::Payload::DismountRequest, Handling::ClientOnly),
        // V28's intent is client-only; its two authoritative answers are consumed.
        (fb::Payload::PlayerTradeRequest, Handling::ClientOnly),
        (fb::Payload::PlayerTradeState, Handling::Consumed),
        (fb::Payload::PlayerTradeClosed, Handling::Consumed),
        // V30's two. The speaker's intent is client-only and always will be. The frame
        // the server chose to relay is read by an arm of its own since the client half
        // of #850; it was `Deferred` through the contract part before that, the staged
        // shape V24's map payloads and V25's stall each had. Nothing plays it yet — the
        // audio path is #851 — and `Consumed` is about the decode boundary rather than
        // about a consumer, which is what `MapTile` meant before the map window existed.
        (fb::Payload::VoiceFrame, Handling::ClientOnly),
        (fb::Payload::VoiceHeard, Handling::Consumed),
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

    #[test]
    fn an_attack_refused_for_ammunition_reaches_the_status_vocabulary() {
        let frame = encode_action_refused(
            fb::RefusedAction::Attack,
            fb::RefusalReason::NoAmmunition,
            None,
        );
        assert_eq!(
            decode(&frame),
            Ok(Message::ActionRefused(ActionRefused {
                action: RefusedAction::Attack,
                reason: RefusalReason::NoAmmunition,
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

    /// V28's player-trade refusal vocabulary is known, while the total conversion still
    /// maps a later contract's member to `Unknown` as the existing sweep above proves.
    #[test]
    fn player_trade_refusals_decode_totally() {
        for (wire, want) in [
            (
                fb::RefusalReason::AlreadyTrading,
                RefusalReason::AlreadyTrading,
            ),
            (fb::RefusalReason::TradeNotOpen, RefusalReason::TradeNotOpen),
            (
                fb::RefusalReason::TradeSlotTaken,
                RefusalReason::TradeSlotTaken,
            ),
            (
                fb::RefusalReason::NothingToOffer,
                RefusalReason::NothingToOffer,
            ),
            (
                fb::RefusalReason::TradeCooldown,
                RefusalReason::TradeCooldown,
            ),
        ] {
            assert_eq!(
                decode(&encode_action_refused(
                    fb::RefusedAction::PlayerTrade,
                    wire,
                    None,
                )),
                Ok(Message::ActionRefused(ActionRefused {
                    action: RefusedAction::PlayerTrade,
                    reason: want,
                    anchor: None,
                }))
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
    /// A tile is placed on a canvas from what it says about itself, so this side holds
    /// the same grid the server's decoder does.
    ///
    /// The scale is checked first because everything else is measured against it — the
    /// origin grid and the mask length both — so a scale with no member leaves nothing
    /// to check the rest against.
    #[test]
    fn a_map_tile_is_held_to_its_grid_its_lengths_and_its_vocabulary() {
        let height = vec![7u8; MAP_TILE_CELLS];
        let mut surface = vec![fb::MapSurface::Grass.0; MAP_TILE_CELLS];
        surface[0] = fb::MapSurface::Unknown.0;
        surface[1] = fb::MapSurface::Settlement.0;
        let explored = vec![0b0000_0011u8; map_tile_explored_bytes(4).expect("scale 4")];

        let frame = encode_map_tile(256, -512, 4, Some(&height), Some(&surface), Some(&explored));
        let Ok(Message::MapTile(tile)) = decode(&frame) else {
            panic!("a well-formed tile was refused: {:?}", decode(&frame));
        };
        assert_eq!((tile.origin_x, tile.origin_z, tile.scale), (256, -512, 4));
        assert_eq!(tile.height.len(), MAP_TILE_CELLS);
        assert_eq!(tile.surface[0], MapSurface::Unknown);
        assert_eq!(tile.surface[1], MapSurface::Settlement);
        assert_eq!(tile.surface[2], MapSurface::Grass);
        assert_eq!(tile.explored, explored);

        // The three scales and the exact mask each one needs. Scale 1 is the case that
        // rounds: four chunk columns do not fill a byte, and the contract says the four
        // unused high bits are zero rather than that the vector is half a byte long.
        for (scale, span, bytes) in [(1u8, 64i32, 1usize), (4, 256, 8), (16, 1024, 128)] {
            assert_eq!(map_tile_span(scale), Some(span), "scale {scale}");
            assert_eq!(map_tile_explored_bytes(scale), Some(bytes), "scale {scale}");
        }
        assert_eq!(map_tile_span(2), None);
        assert_eq!(map_tile_explored_bytes(0), None);

        let short = vec![0u8; MAP_TILE_CELLS - 1];
        let mask4 = vec![0u8; 8];
        for (name, frame, want) in [
            (
                "absent scale",
                encode_map_tile(0, 0, 0, Some(&height), Some(&surface), Some(&[0])),
                DecodeError::MapTileScale(0),
            ),
            (
                "a scale with no member",
                encode_map_tile(0, 0, 3, Some(&height), Some(&surface), Some(&[0])),
                DecodeError::MapTileScale(3),
            ),
            (
                "an origin off the grid",
                encode_map_tile(64, 0, 4, Some(&height), Some(&surface), Some(&mask4)),
                DecodeError::MapTileOffGrid {
                    origin_x: 64,
                    origin_z: 0,
                    scale: 4,
                },
            ),
            (
                "a short height array",
                encode_map_tile(0, 0, 4, Some(&short), Some(&surface), Some(&mask4)),
                DecodeError::MapTileArrayLength {
                    field: "height",
                    len: MAP_TILE_CELLS - 1,
                    want: MAP_TILE_CELLS,
                },
            ),
            (
                "no surface array at all",
                encode_map_tile(0, 0, 4, Some(&height), None, Some(&mask4)),
                DecodeError::MapTileArrayLength {
                    field: "surface",
                    len: 0,
                    want: MAP_TILE_CELLS,
                },
            ),
            (
                "a mask sized for another scale",
                encode_map_tile(0, 0, 4, Some(&height), Some(&surface), Some(&[0])),
                DecodeError::MapTileArrayLength {
                    field: "explored",
                    len: 1,
                    want: 8,
                },
            ),
        ] {
            assert_eq!(decode(&frame), Err(want), "{name}");
        }

        // A surface byte this build cannot name is refused rather than drawn. The map is
        // the one place a guessed value would be shown to a player as fact.
        let mut unknown = surface.clone();
        unknown[9] = 200;
        assert_eq!(
            decode(&encode_map_tile(
                0,
                0,
                4,
                Some(&height),
                Some(&unknown),
                Some(&mask4)
            )),
            Err(DecodeError::UnknownMapSurface {
                index: 9,
                value: 200
            })
        );
    }

    /// The ledger is additive, so an empty page is refused and a repeated column is not.
    ///
    /// The pair is the point: a column named twice has told this client the same true
    /// thing twice, where an empty page read as "the ledger is empty" would erase a map.
    #[test]
    fn an_explored_page_is_bounded_and_never_empty() {
        let Ok(Message::MapExplored(page)) =
            decode(&encode_map_explored(Some(&[(0, 0), (-3, 91), (0, 0)])))
        else {
            panic!("a well-formed page was refused");
        };
        assert_eq!(
            page.columns,
            vec![
                MapColumn { cx: 0, cz: 0 },
                MapColumn { cx: -3, cz: 91 },
                MapColumn { cx: 0, cz: 0 },
            ]
        );

        assert_eq!(
            decode(&encode_map_explored(Some(&[]))),
            Err(DecodeError::MapExploredWithoutColumns)
        );
        assert_eq!(
            decode(&encode_map_explored(None)),
            Err(DecodeError::MapExploredWithoutColumns)
        );

        let full: Vec<(i32, i32)> = (0..MAX_EXPLORED_COLUMNS as i32).map(|i| (i, 0)).collect();
        assert!(matches!(
            decode(&encode_map_explored(Some(&full))),
            Ok(Message::MapExplored(_))
        ));
        let over: Vec<(i32, i32)> = (0..MAX_EXPLORED_COLUMNS as i32 + 1)
            .map(|i| (i, 0))
            .collect();
        assert_eq!(
            decode(&encode_map_explored(Some(&over))),
            Err(DecodeError::MapExploredTooManyColumns(
                MAX_EXPLORED_COLUMNS + 1
            ))
        );
    }

    /// The complete list replaces the client's copy, so every bound it carries is held
    /// here — and an empty list is the ordinary case rather than a refusal.
    #[test]
    fn a_marker_list_replaces_the_map_and_is_bounded_at_the_decode_boundary() {
        let marks = [
            MarkerWire {
                marker_id: 1,
                x: 10,
                z: -10,
                kind: fb::MarkerKind::Boss.0,
                note: Some("the draugr"),
            },
            MarkerWire {
                marker_id: 2,
                x: 0,
                z: 0,
                kind: fb::MarkerKind::Note.0,
                note: None,
            },
        ];
        let Ok(Message::MarkerList(list)) = decode(&encode_marker_list(Some(&marks))) else {
            panic!("a well-formed list was refused");
        };
        assert_eq!(
            list.markers,
            vec![
                Marker {
                    marker_id: 1,
                    x: 10,
                    z: -10,
                    kind: MarkerKind::Boss,
                    note: "the draugr".to_owned(),
                },
                // Absent and empty are the same empty note.
                Marker {
                    marker_id: 2,
                    x: 0,
                    z: 0,
                    kind: MarkerKind::Note,
                    note: String::new(),
                },
            ]
        );

        // A character who has marked nothing, and a message that has none of the vector
        // at all: both are an empty map rather than an error.
        for frame in [encode_marker_list(Some(&[])), encode_marker_list(None)] {
            assert_eq!(
                decode(&frame),
                Ok(Message::MarkerList(MarkerList { markers: vec![] }))
            );
        }

        let one = |marker_id, kind, note| MarkerWire {
            marker_id,
            x: 0,
            z: 0,
            kind,
            note,
        };
        let long = "a".repeat(MARKER_NOTE_MAX_BYTES + 1);
        // Forty three-byte runes are exactly the bound; forty-one are 123 bytes and are
        // not. A bound counted in characters would have accepted the second.
        let runes = "\u{16D7}".repeat(40);
        let over_runes = "\u{16D7}".repeat(41);
        for (name, marks, want) in [
            (
                "the reserved id",
                vec![one(0, fb::MarkerKind::Camp.0, None)],
                DecodeError::MarkerWithoutIdentity,
            ),
            (
                "one id twice",
                vec![
                    one(5, fb::MarkerKind::Camp.0, None),
                    one(5, fb::MarkerKind::Cave.0, None),
                ],
                DecodeError::DuplicateMarker(5),
            ),
            (
                "the absent-field kind",
                vec![one(5, fb::MarkerKind::Unknown.0, None)],
                DecodeError::UnknownMarkerKind {
                    marker_id: 5,
                    value: 0,
                },
            ),
            (
                "a kind with no member",
                vec![one(5, 200, None)],
                DecodeError::UnknownMarkerKind {
                    marker_id: 5,
                    value: 200,
                },
            ),
            (
                "a note past the bound",
                vec![one(5, fb::MarkerKind::Note.0, Some(&long))],
                DecodeError::MarkerNoteTooLong {
                    marker_id: 5,
                    len: MARKER_NOTE_MAX_BYTES + 1,
                },
            ),
            (
                "a multibyte note past the bound",
                vec![one(5, fb::MarkerKind::Note.0, Some(&over_runes))],
                DecodeError::MarkerNoteTooLong {
                    marker_id: 5,
                    len: 123,
                },
            ),
        ] {
            assert_eq!(
                decode(&encode_marker_list(Some(&marks))),
                Err(want),
                "{name}"
            );
        }
        assert!(
            matches!(
                decode(&encode_marker_list(Some(&[one(
                    5,
                    fb::MarkerKind::Note.0,
                    Some(&runes)
                )]))),
                Ok(Message::MarkerList(_))
            ),
            "forty three-byte runes are exactly {MARKER_NOTE_MAX_BYTES} bytes"
        );

        let full: Vec<_> = (1..=MAX_MARKERS as u64)
            .map(|id| one(id, fb::MarkerKind::Camp.0, None))
            .collect();
        assert!(matches!(
            decode(&encode_marker_list(Some(&full))),
            Ok(Message::MarkerList(_))
        ));
        let over: Vec<_> = (1..=MAX_MARKERS as u64 + 1)
            .map(|id| one(id, fb::MarkerKind::Camp.0, None))
            .collect();
        assert_eq!(
            decode(&encode_marker_list(Some(&over))),
            Err(DecodeError::TooManyMarkers(MAX_MARKERS + 1))
        );
    }

    /// A note that is not UTF-8 is refused before the accessor that would read it runs.
    ///
    /// The same property as
    /// [`a_character_name_that_is_not_utf8_is_refused_before_the_accessor_runs`], pinned
    /// again for the one string this protocol version adds. `Marker::note()` is
    /// `from_utf8_unchecked` like every other generated string accessor, so what stands
    /// between a hostile frame and undefined behaviour is that [`decode`] goes through
    /// `root_as_envelope`: the generated verifier visits `note` as
    /// `ForwardsUOffset<&str>`, and that impl runs `core::str::from_utf8` and returns
    /// `InvalidFlatbuffer::Utf8Error`. The bound in [`marker_list`] is a *length* check
    /// and is reached only afterwards — it is not what makes the read safe, and reading
    /// the code cannot show that it does not have to be.
    ///
    /// So this is pinned for the same reason #117's review asked for the first one:
    /// the guarantee is library behaviour plus the "never `root_as_envelope_unchecked`"
    /// convention plus a pinned `Cargo.lock`, and a convention is what a regression walks
    /// through. The bytes are patched into a finished frame rather than built through
    /// `from_utf8_unchecked`, because `client/Cargo.toml` records that hand-written client
    /// code contains no `unsafe` and a safety test is a poor place to write the first.
    #[test]
    fn a_marker_note_that_is_not_utf8_is_refused_before_the_accessor_runs() {
        // Distinctive enough to appear once in a frame, and checked below rather than
        // assumed. 0xC3 opens a two-byte sequence and 0x28 is not a continuation byte.
        const NOTE: &[u8] = b"Qxvz";
        const NOT_UTF8: &[u8] = &[0xC3, 0x28];

        let mut frame = encode_marker_list(Some(&[MarkerWire {
            marker_id: 7,
            x: 0,
            z: 0,
            kind: fb::MarkerKind::Note.0,
            note: Some(core::str::from_utf8(NOTE).expect("the fixture note is ascii")),
        }]));

        // The frame this patches is a good one, which is what makes the refusal below a
        // statement about the bytes rather than about the fixture.
        assert!(
            decode(&frame).is_ok(),
            "the unpatched fixture is not a decodable frame"
        );

        let occurrences = frame.windows(NOTE.len()).filter(|w| *w == NOTE).count();
        assert_eq!(occurrences, 1, "the fixture note is not uniquely locatable");
        let at = frame
            .windows(NOTE.len())
            .position(|window| window == NOTE)
            .expect("the note is in the frame it was encoded into");
        // The replacement is the same length as what it replaces, so no offset moves.
        frame[at..at + NOT_UTF8.len()].copy_from_slice(NOT_UTF8);

        // The reason is pinned, not just the refusal: a patched buffer could in principle
        // be refused for something that has nothing to do with UTF-8, which would leave
        // this test passing while the property it exists for went unchecked. It must also
        // not be `MarkerNoteTooLong` — that would mean the length check had run first, on
        // a `&str` the accessor had already fabricated.
        let refusal = decode(&frame);
        let Err(DecodeError::Malformed(reason)) = &refusal else {
            panic!("invalid UTF-8 in a note was not refused: {refusal:?}");
        };
        assert!(
            reason.contains("Utf8") && reason.contains("note"),
            "the frame was refused for something other than the note's encoding: {reason}"
        );
    }

    /// A resident arrives named, roled and described, and every one of those is bounded
    /// here rather than downstream.
    ///
    /// The name is refused *empty* as well as absent, which is where this differs from
    /// [`PlayerAppearance`]: a player's stored name is copied verbatim from what the
    /// server holds and may legitimately be empty, while a resident is named by the world
    /// that placed them.
    #[test]
    fn a_resident_arrives_named_and_roled_and_is_bounded_at_the_decode_boundary() {
        let frame = encode_resident_appearance(
            900,
            Some("Ingrid"),
            fb::ResidentRole::Stablemaster.0,
            Some(AppearanceWire::default()),
        );
        let Ok(Message::ResidentAppearance(resident)) = decode(&frame) else {
            panic!("a well-formed resident was refused");
        };
        assert_eq!(resident.entity_id, 900);
        assert_eq!(resident.name, "Ingrid");
        assert_eq!(resident.role, ResidentRole::Stablemaster);

        // Every role this part consumes decodes. The absent-field zero and a value beyond
        // this contract still fail closed.
        for (wire, want) in [
            (fb::ResidentRole::Villager, ResidentRole::Villager),
            (fb::ResidentRole::Smith, ResidentRole::Smith),
            (fb::ResidentRole::Carpenter, ResidentRole::Carpenter),
            (fb::ResidentRole::Cook, ResidentRole::Cook),
            (fb::ResidentRole::Trader, ResidentRole::Trader),
            (fb::ResidentRole::Guard, ResidentRole::Guard),
            (fb::ResidentRole::Stablemaster, ResidentRole::Stablemaster),
        ] {
            assert_eq!(ResidentRole::from_wire(wire), Some(want));
        }
        assert_eq!(ResidentRole::from_wire(fb::ResidentRole::Unknown), None);
        assert_eq!(ResidentRole::from_wire(fb::ResidentRole(8)), None);

        // Exactly at the bound is accepted. Bytes, not characters: eleven three-byte
        // runes are 33 bytes and are refused, while a 32-character ASCII name is not.
        let role = fb::ResidentRole::Guard.0;
        let at_bound = "a".repeat(RESIDENT_NAME_MAX_BYTES);
        assert!(
            decode(&encode_resident_appearance(
                900,
                Some(&at_bound),
                role,
                Some(AppearanceWire::default())
            ))
            .is_ok(),
            "a {RESIDENT_NAME_MAX_BYTES}-byte name was refused"
        );

        for (name, frame, want) in [
            (
                "no entity",
                encode_resident_appearance(
                    0,
                    Some("Ingrid"),
                    role,
                    Some(AppearanceWire::default()),
                ),
                DecodeError::ResidentWithoutEntity,
            ),
            (
                "absent name",
                encode_resident_appearance(900, None, role, Some(AppearanceWire::default())),
                DecodeError::ResidentWithoutName(900),
            ),
            (
                "empty name",
                encode_resident_appearance(900, Some(""), role, Some(AppearanceWire::default())),
                DecodeError::ResidentWithoutName(900),
            ),
            (
                "name one byte past the bound",
                encode_resident_appearance(
                    900,
                    Some(&"a".repeat(RESIDENT_NAME_MAX_BYTES + 1)),
                    role,
                    Some(AppearanceWire::default()),
                ),
                DecodeError::ResidentNameTooLong {
                    entity_id: 900,
                    len: RESIDENT_NAME_MAX_BYTES + 1,
                },
            ),
            (
                "multibyte name past the bound",
                encode_resident_appearance(
                    900,
                    Some(&"ᛗ".repeat(11)),
                    role,
                    Some(AppearanceWire::default()),
                ),
                DecodeError::ResidentNameTooLong {
                    entity_id: 900,
                    len: 33,
                },
            ),
            (
                "absent role",
                encode_resident_appearance(
                    900,
                    Some("Ingrid"),
                    fb::ResidentRole::Unknown.0,
                    Some(AppearanceWire::default()),
                ),
                DecodeError::UnknownResidentRole {
                    entity_id: 900,
                    value: 0,
                },
            ),
            (
                "role from a newer contract",
                encode_resident_appearance(
                    900,
                    Some("Ingrid"),
                    200,
                    Some(AppearanceWire::default()),
                ),
                DecodeError::UnknownResidentRole {
                    entity_id: 900,
                    value: 200,
                },
            ),
        ] {
            assert_eq!(decode(&frame), Err(want), "{name} was not refused");
        }

        // An appearance that is absent is refused by the shared helper, exactly as a
        // player's is: a client may not invent what the server did not describe.
        assert!(
            decode(&encode_resident_appearance(900, Some("Ingrid"), role, None)).is_err(),
            "a resident with no appearance was accepted"
        );

        // Ten three-byte runes are 30 bytes and fit, which is what makes the eleven-rune
        // refusal above a statement about bytes rather than about that particular string.
        assert!(
            decode(&encode_resident_appearance(
                900,
                Some(&"ᛗ".repeat(10)),
                role,
                Some(AppearanceWire::default())
            ))
            .is_ok(),
            "ten three-byte runes are 30 bytes and were refused"
        );
    }

    /// A resident name that is not UTF-8 is refused before the accessor that would read
    /// it runs.
    ///
    /// The third instance of the property
    /// [`a_character_name_that_is_not_utf8_is_refused_before_the_accessor_runs`] pins and
    /// [`a_marker_note_that_is_not_utf8_is_refused_before_the_accessor_runs`] pinned
    /// again, here for the one string this protocol version adds.
    /// `ResidentAppearance::name()` is `from_utf8_unchecked` like every other generated
    /// string accessor, so what stands between a hostile frame and undefined behaviour is
    /// that [`decode`] goes through `root_as_envelope`: the generated verifier visits
    /// `name` as `ForwardsUOffset<&str>`, and that impl runs `core::str::from_utf8` and
    /// returns `InvalidFlatbuffer::Utf8Error` before any accessor is called.
    ///
    /// The `RESIDENT_NAME_MAX_BYTES` bound in the decode arm is a *length* check reached
    /// only afterwards — it is not what makes the read safe, and reading the code cannot
    /// show that it does not have to be. That is the whole reason this is pinned rather
    /// than left to library behaviour plus the "never `root_as_envelope_unchecked`"
    /// convention plus a pinned `Cargo.lock`: two of those three are conventions, and a
    /// convention is what a regression walks through.
    ///
    /// The bytes are patched into a finished frame rather than built through
    /// `from_utf8_unchecked`, because `client/Cargo.toml` records that hand-written client
    /// code contains no `unsafe` and a safety test is a poor place to write the first.
    #[test]
    fn a_resident_name_that_is_not_utf8_is_refused_before_the_accessor_runs() {
        // Distinctive enough to appear once in a frame, and checked below rather than
        // assumed. 0xC3 opens a two-byte sequence and 0x28 is not a continuation byte.
        const NAME: &[u8] = b"Qxvz";
        const NOT_UTF8: &[u8] = &[0xC3, 0x28];

        let mut frame = encode_resident_appearance(
            900,
            Some(core::str::from_utf8(NAME).expect("the fixture name is ascii")),
            fb::ResidentRole::Smith.0,
            Some(AppearanceWire::default()),
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
        // Two of the four bytes, overwritten in place: the replacement is the same length
        // as the slice it replaces, so the string's length prefix and every offset after it
        // stay exactly where the encoder put them.
        frame[at..at + NOT_UTF8.len()].copy_from_slice(NOT_UTF8);

        // The reason is pinned, not just the refusal: a patched buffer could in principle
        // be refused for something that has nothing to do with UTF-8, which would leave
        // this test passing while the property it exists for went unchecked. It must also
        // not be `ResidentWithoutName` or `ResidentNameTooLong` — either would mean this
        // arm's own checks had run first, on a `&str` the accessor had already fabricated.
        let refusal = decode(&frame);
        let Err(DecodeError::Malformed(reason)) = &refusal else {
            panic!("invalid UTF-8 in a resident name was not refused: {refusal:?}");
        };
        assert!(
            reason.contains("Utf8") && reason.contains("name"),
            "the frame was refused for something other than the name's encoding: {reason}"
        );
    }

    /// A price list replaces the client's view of one vendor, so every bound it carries
    /// is held here.
    ///
    /// **One item at two prices in the two directions is the ordinary case**, and the
    /// uniqueness check is deliberately per vector for that reason: a stall that sells
    /// iron at 12 and buys it at 5 is a stall, and the spread is the whole of what one is.
    #[test]
    fn a_vendor_state_replaces_the_prices_and_is_bounded_at_the_decode_boundary() {
        let frame = encode_vendor_state(900, 3, Some(&[(31, 12), (8, 40)]), Some(&[(31, 5)]));
        assert_eq!(
            decode(&frame),
            Ok(Message::VendorState(VendorState {
                entity_id: 900,
                revision: 3,
                sells: vec![
                    VendorEntry {
                        item_id: 31,
                        price: 12
                    },
                    VendorEntry {
                        item_id: 8,
                        price: 40
                    },
                ],
                buys: vec![VendorEntry {
                    item_id: 31,
                    price: 5
                }],
            }))
        );

        // A vendor that only sells and one that only buys are both ordinary, and both
        // carry the other vector present and empty.
        assert_eq!(
            decode(&encode_vendor_state(900, 1, Some(&[(31, 12)]), Some(&[]))),
            Ok(Message::VendorState(VendorState {
                entity_id: 900,
                revision: 1,
                sells: vec![VendorEntry {
                    item_id: 31,
                    price: 12
                }],
                buys: vec![],
            }))
        );

        for (name, frame, want) in [
            (
                "no entity",
                encode_vendor_state(0, 1, Some(&[(31, 12)]), Some(&[])),
                DecodeError::VendorWithoutEntity("VendorState"),
            ),
            (
                "no revision",
                encode_vendor_state(900, 0, Some(&[(31, 12)]), Some(&[])),
                DecodeError::VendorWithoutRevision,
            ),
            (
                "absent sells",
                encode_vendor_state(900, 1, None, Some(&[(31, 5)])),
                DecodeError::VendorWithoutPrices {
                    entity_id: 900,
                    field: "sells",
                },
            ),
            (
                "absent buys",
                encode_vendor_state(900, 1, Some(&[(31, 12)]), None),
                DecodeError::VendorWithoutPrices {
                    entity_id: 900,
                    field: "buys",
                },
            ),
            (
                "nothing in either direction",
                encode_vendor_state(900, 1, Some(&[]), Some(&[])),
                DecodeError::VendorWithNothingToTrade(900),
            ),
            (
                "item 0",
                encode_vendor_state(900, 1, Some(&[(0, 12)]), Some(&[])),
                DecodeError::VendorEntryWithoutItem {
                    entity_id: 900,
                    field: "sells",
                },
            ),
            (
                "free is not a price",
                encode_vendor_state(900, 1, Some(&[]), Some(&[(31, 0)])),
                DecodeError::VendorEntryWithoutPrice {
                    entity_id: 900,
                    field: "buys",
                    item_id: 31,
                },
            ),
            (
                "one item at two prices in one direction",
                encode_vendor_state(900, 1, Some(&[(31, 12), (31, 13)]), Some(&[])),
                DecodeError::DuplicateVendorEntry {
                    entity_id: 900,
                    field: "sells",
                    item_id: 31,
                },
            ),
        ] {
            assert_eq!(decode(&frame), Err(want), "{name} was not refused");
        }

        assert_eq!(
            decode(&encode_vendor_closed(900)),
            Ok(Message::VendorClosed(VendorClosed { entity_id: 900 }))
        );
        assert_eq!(
            decode(&encode_vendor_closed(0)),
            Err(DecodeError::VendorWithoutEntity("VendorClosed"))
        );
    }

    #[test]
    fn a_player_trade_state_is_complete_and_empty_offers_are_legal() {
        let my_offer = [PlayerTradeSlotWire {
            trade_slot: 2,
            pack_slot: 9,
            item_id: 31,
            count: 1,
            durability: 17,
            max_durability: 25,
        }];
        let their_offer = [PlayerTradeSlotWire {
            trade_slot: 4,
            pack_slot: 0,
            item_id: 8,
            count: 3,
            durability: 0,
            max_durability: 0,
        }];
        assert_eq!(
            decode(&encode_player_trade_state(
                77,
                Some(""),
                4,
                Some(&my_offer),
                Some(&their_offer),
                (120, 35),
                (true, false),
            )),
            Ok(Message::PlayerTradeState(PlayerTradeState {
                partner_entity_id: 77,
                partner_name: String::new(),
                revision: 4,
                my_offer: vec![PlayerTradeSlot {
                    trade_slot: 2,
                    pack_slot: 9,
                    item_id: 31,
                    count: 1,
                    durability: 17,
                    max_durability: 25,
                }],
                their_offer: vec![PlayerTradeSlot {
                    trade_slot: 4,
                    pack_slot: 0,
                    item_id: 8,
                    count: 3,
                    durability: 0,
                    max_durability: 0,
                }],
                my_silver: 120,
                their_silver: 35,
                my_confirmed: true,
                their_confirmed: false,
            }))
        );

        assert_eq!(
            decode(&encode_player_trade_state(
                77,
                Some("Eydis"),
                1,
                Some(&[]),
                Some(&[]),
                (0, 0),
                (false, false),
            )),
            Ok(Message::PlayerTradeState(PlayerTradeState {
                partner_entity_id: 77,
                partner_name: "Eydis".to_owned(),
                revision: 1,
                my_offer: vec![],
                their_offer: vec![],
                my_silver: 0,
                their_silver: 0,
                my_confirmed: false,
                their_confirmed: false,
            }))
        );
    }

    /// Holds every vector invariant, including the partner's private pack slot.
    #[test]
    fn malformed_player_trade_states_are_refused() {
        let ordinary = PlayerTradeSlotWire::default();
        let partner = PlayerTradeSlotWire {
            pack_slot: 0,
            ..ordinary
        };
        let six = [ordinary; PLAYER_TRADE_SLOTS + 1];
        let duplicate = [
            ordinary,
            PlayerTradeSlotWire {
                pack_slot: 8,
                ..ordinary
            },
        ];
        let out_of_range = [PlayerTradeSlotWire {
            trade_slot: PLAYER_TRADE_SLOTS as u8,
            ..ordinary
        }];
        let empty = [PlayerTradeSlotWire {
            count: 0,
            ..ordinary
        }];
        let no_maximum = [PlayerTradeSlotWire {
            durability: 2,
            max_durability: 0,
            ..ordinary
        }];
        let past_maximum = [PlayerTradeSlotWire {
            durability: 11,
            max_durability: 10,
            ..ordinary
        }];
        let durable_pair = [PlayerTradeSlotWire {
            count: 2,
            durability: 4,
            max_durability: 10,
            ..ordinary
        }];

        for (name, frame, want) in [
            (
                "no partner",
                encode_player_trade_state(
                    0,
                    Some("Eydis"),
                    1,
                    Some(&[]),
                    Some(&[]),
                    (0, 0),
                    (false, false),
                ),
                DecodeError::PlayerTradeWithoutPartner("PlayerTradeState"),
            ),
            (
                "no partner name",
                encode_player_trade_state(
                    77,
                    None,
                    1,
                    Some(&[]),
                    Some(&[]),
                    (0, 0),
                    (false, false),
                ),
                DecodeError::PlayerTradeWithoutPartnerName,
            ),
            (
                "no revision",
                encode_player_trade_state(
                    77,
                    Some("Eydis"),
                    0,
                    Some(&[]),
                    Some(&[]),
                    (0, 0),
                    (false, false),
                ),
                DecodeError::PlayerTradeWithoutRevision,
            ),
            (
                "no own offer",
                trade_state(None, Some(&[])),
                DecodeError::PlayerTradeWithoutOffer("my_offer"),
            ),
            (
                "no partner offer",
                trade_state(Some(&[]), None),
                DecodeError::PlayerTradeWithoutOffer("their_offer"),
            ),
            (
                "six own entries",
                trade_state(Some(&six), Some(&[])),
                DecodeError::PlayerTradeOfferTooLarge {
                    field: "my_offer",
                    len: 6,
                },
            ),
            (
                "duplicate own trade slot",
                trade_state(Some(&duplicate), Some(&[])),
                DecodeError::DuplicatePlayerTradeSlot {
                    field: "my_offer",
                    trade_slot: 0,
                },
            ),
            (
                "trade slot five",
                trade_state(Some(&out_of_range), Some(&[])),
                DecodeError::PlayerTradeSlotOutOfRange {
                    field: "my_offer",
                    index: 0,
                    trade_slot: 5,
                },
            ),
            (
                "zero count",
                trade_state(Some(&empty), Some(&[])),
                DecodeError::EmptyPlayerTradeSlot {
                    field: "my_offer",
                    index: 0,
                },
            ),
            (
                "durability without maximum",
                trade_state(Some(&no_maximum), Some(&[])),
                DecodeError::PlayerTradeDurabilityWithoutMaximum {
                    field: "my_offer",
                    index: 0,
                    durability: 2,
                },
            ),
            (
                "durability past maximum",
                trade_state(Some(&past_maximum), Some(&[])),
                DecodeError::PlayerTradeDurabilityExceedsMaximum {
                    field: "my_offer",
                    index: 0,
                    durability: 11,
                    max_durability: 10,
                },
            ),
            (
                "durable stack count two",
                trade_state(Some(&durable_pair), Some(&[])),
                DecodeError::PlayerTradeDurableStackCount {
                    field: "my_offer",
                    index: 0,
                    count: 2,
                },
            ),
            (
                "partner pack slot exposed",
                trade_state(Some(&[]), Some(&[ordinary])),
                DecodeError::PlayerTradePartnerPackSlot {
                    index: 0,
                    pack_slot: 7,
                },
            ),
        ] {
            assert_eq!(decode(&frame), Err(want), "{name} was not refused");
        }

        // The same stack is legal on their side once the private pack position is zero.
        assert!(matches!(
            decode(&trade_state(Some(&[]), Some(&[partner]))),
            Ok(Message::PlayerTradeState(_))
        ));
    }

    #[test]
    fn player_trade_close_reasons_decode_totally() {
        for (wire, want) in [
            (0, PlayerTradeCloseReason::Unknown),
            (1, PlayerTradeCloseReason::Completed),
            (2, PlayerTradeCloseReason::Cancelled),
            (3, PlayerTradeCloseReason::OutOfReach),
            (4, PlayerTradeCloseReason::Died),
            (5, PlayerTradeCloseReason::Disconnected),
            (6, PlayerTradeCloseReason::Failed),
            (200, PlayerTradeCloseReason::Unknown),
        ] {
            assert_eq!(
                decode(&encode_player_trade_closed(77, wire)),
                Ok(Message::PlayerTradeClosed(PlayerTradeClosed {
                    partner_entity_id: 77,
                    reason: want,
                }))
            );
        }
        assert_eq!(
            decode(&encode_player_trade_closed(0, 1)),
            Err(DecodeError::PlayerTradeWithoutPartner("PlayerTradeClosed"))
        );
    }

    /// A warning says where the storm is, and the number beside the phase is read as
    /// whatever that phase makes it.
    ///
    /// The phase is the only thing here with a vocabulary, and it carries the whole
    /// message: a countdown, a remaining duration and a zero are three quantities wearing
    /// one field. So the zero member is refused like every other absent-field zero, and
    /// the one cross-field rule the contract states — a passed storm carries 0 — is
    /// refused rather than repaired, because a `Passed` with 300 seconds on it is two
    /// statements about two different storms.
    #[test]
    fn a_storm_warning_says_which_number_it_is_carrying() {
        for (seconds_until, phase, want) in [
            (300u32, fb::StormPhase::Approaching, StormPhase::Approaching),
            (0, fb::StormPhase::Approaching, StormPhase::Approaching),
            (45, fb::StormPhase::Raging, StormPhase::Raging),
            (0, fb::StormPhase::Raging, StormPhase::Raging),
            (0, fb::StormPhase::Passed, StormPhase::Passed),
        ] {
            assert_eq!(
                decode(&encode_storm_warning(seconds_until, phase.0)),
                Ok(Message::StormWarning(StormWarning {
                    seconds_until,
                    phase: want
                })),
                "{phase:?} with {seconds_until}s did not survive the wire"
            );
        }

        // Unbounded above by this contract, deliberately: seconds are not ticks, and
        // nothing here has a day length to measure them against.
        assert_eq!(
            decode(&encode_storm_warning(u32::MAX, fb::StormPhase::Raging.0)),
            Ok(Message::StormWarning(StormWarning {
                seconds_until: u32::MAX,
                phase: StormPhase::Raging
            }))
        );

        assert_eq!(
            decode(&encode_storm_warning(0, fb::StormPhase::Unknown.0)),
            Err(DecodeError::UnknownStormPhase { value: 0 }),
            "the absent-field zero is a defect, not a phase"
        );
        assert_eq!(
            decode(&encode_storm_warning(0, 9)),
            Err(DecodeError::UnknownStormPhase { value: 9 }),
            "a phase from a newer contract has no number to read seconds_until as"
        );
        assert_eq!(
            decode(&encode_storm_warning(300, fb::StormPhase::Passed.0)),
            Err(DecodeError::StormPassedWithCountdown { seconds_until: 300 }),
            "a storm that is over does not still count down"
        );
    }

    /// The warded columns are a complete set, and an empty one is the message that says
    /// the player has walked out of the last ward.
    ///
    /// That is why absence and empty are one case here and not two — [`MarkerList`]'s
    /// rule — where an empty `MapExplored` page is refused: this message *replaces* the
    /// client's set, so an empty one states something, and a page that adds nothing does
    /// not.
    #[test]
    fn wards_are_a_complete_set_and_an_empty_one_is_a_statement() {
        let columns = [
            WardedColumnWire {
                cx: -3,
                cz: 7,
                kind: fb::WardKind::Runestone.0,
                mine: true,
            },
            WardedColumnWire {
                cx: -3,
                cz: 8,
                kind: fb::WardKind::Runestone.0,
                mine: false,
            },
            WardedColumnWire {
                cx: 40,
                cz: -12,
                kind: fb::WardKind::Settlement.0,
                mine: false,
            },
        ];
        assert_eq!(
            decode(&encode_wards_nearby(Some(&columns))),
            Ok(Message::WardsNearby(WardsNearby {
                columns: vec![
                    WardedColumn {
                        cx: -3,
                        cz: 7,
                        kind: WardKind::Runestone,
                        mine: true,
                    },
                    WardedColumn {
                        cx: -3,
                        cz: 8,
                        kind: WardKind::Runestone,
                        mine: false,
                    },
                    WardedColumn {
                        cx: 40,
                        cz: -12,
                        kind: WardKind::Settlement,
                        mine: false,
                    },
                ],
            }))
        );

        // Both spellings of "no wards in view", and they are the same message.
        for (name, frame) in [
            ("an absent vector", encode_wards_nearby(None)),
            ("a present empty vector", encode_wards_nearby(Some(&[]))),
        ] {
            assert_eq!(
                decode(&frame),
                Ok(Message::WardsNearby(WardsNearby { columns: vec![] })),
                "{name} is how a client learns it has walked out of the last ward"
            );
        }

        assert_eq!(
            MAX_WARDED_COLUMNS, 2048,
            "the bound is the contract's own WardBound, and a change to it is a contract change"
        );
        let at_the_bound: Vec<_> = (0..MAX_WARDED_COLUMNS)
            .map(|index| WardedColumnWire {
                cx: index as i32,
                cz: 0,
                kind: fb::WardKind::Settlement.0,
                mine: false,
            })
            .collect();
        let Ok(Message::WardsNearby(full)) = decode(&encode_wards_nearby(Some(&at_the_bound)))
        else {
            panic!("a set exactly at the bound is legal");
        };
        assert_eq!(full.columns.len(), MAX_WARDED_COLUMNS);

        let mut over = at_the_bound.clone();
        over.push(WardedColumnWire {
            cx: MAX_WARDED_COLUMNS as i32,
            cz: 0,
            kind: fb::WardKind::Settlement.0,
            mine: false,
        });
        assert_eq!(
            decode(&encode_wards_nearby(Some(&over))),
            Err(DecodeError::TooManyWardedColumns(MAX_WARDED_COLUMNS + 1)),
            "an oversized set is refused rather than truncated: a dropped tail shades the \
             world wrong and says nothing"
        );

        for (name, kind, value) in [
            ("the absent-field zero", fb::WardKind::Unknown.0, 0u8),
            ("a kind from a newer contract", 9, 9),
        ] {
            assert_eq!(
                decode(&encode_wards_nearby(Some(&[WardedColumnWire {
                    cx: 2,
                    cz: -5,
                    kind,
                    mine: false,
                }]))),
                Err(DecodeError::UnknownWardKind {
                    cx: 2,
                    cz: -5,
                    value
                }),
                "{name} would shade ground whose claim this build cannot name"
            );
        }

        assert_eq!(
            decode(&encode_wards_nearby(Some(&[
                WardedColumnWire {
                    cx: 2,
                    cz: -5,
                    kind: fb::WardKind::Runestone.0,
                    mine: true,
                },
                WardedColumnWire {
                    cx: 2,
                    cz: -5,
                    kind: fb::WardKind::Settlement.0,
                    mine: false,
                },
            ]))),
            Err(DecodeError::DuplicateWardedColumn { cx: 2, cz: -5 }),
            "two rows for one column are two answers about the same ground"
        );
    }

    /// The weather is a struct, so "this server keeps none" is the field being absent —
    /// and a struct that *arrived* has to name a sky.
    ///
    /// The two refusals are the pair the contract states. `Unknown` is the absent-field
    /// zero arriving inside a present struct, which is a defect rather than a silence,
    /// because the silence already has its own spelling. A `Clear` with a non-zero
    /// intensity is refused rather than clamped to either half, for the reason a broken
    /// world clock is refused rather than repaired: the two fields describe different
    /// skies and neither reading is knowably the server's.
    #[test]
    fn the_weather_is_where_the_recipient_stands_and_absence_is_a_server_without_one() {
        let Ok(Message::Snapshot(snapshot)) = decode(
            &encode_entity_snapshot_with_weather_and_bare_structure(None, None),
        ) else {
            panic!("a snapshot with no weather field is an ordinary snapshot");
        };
        assert_eq!(
            snapshot.weather, None,
            "an absent struct is a server that keeps no weather, not a defect"
        );

        for (kind, intensity, want) in [
            (fb::WeatherKind::Clear, 0u8, WeatherKind::Clear),
            (fb::WeatherKind::Rain, 1, WeatherKind::Rain),
            (fb::WeatherKind::Rain, 255, WeatherKind::Rain),
            (fb::WeatherKind::Snow, 128, WeatherKind::Snow),
            (fb::WeatherKind::Sandstorm, 200, WeatherKind::Sandstorm),
            (fb::WeatherKind::Blizzard, 255, WeatherKind::Blizzard),
            // 0 is a legal intensity for every kind, not only for Clear: it is "none of
            // it right now", and nothing here divides the range into named bands.
            (fb::WeatherKind::Snow, 0, WeatherKind::Snow),
        ] {
            let Ok(Message::Snapshot(snapshot)) =
                decode(&encode_entity_snapshot_with_weather_and_bare_structure(
                    Some((kind.0, intensity)),
                    None,
                ))
            else {
                panic!("{kind:?} at {intensity} did not survive the wire");
            };
            assert_eq!(
                snapshot.weather,
                Some(WeatherState {
                    kind: want,
                    intensity
                })
            );
        }

        for (name, kind, value) in [
            ("the absent-field zero", fb::WeatherKind::Unknown.0, 0u8),
            ("a kind from a newer contract", 9, 9),
        ] {
            assert_eq!(
                decode(&encode_entity_snapshot_with_weather_and_bare_structure(
                    Some((kind, 40)),
                    None
                )),
                Err(DecodeError::UnknownWeatherKind { value }),
                "{name} inside a present struct is a defect, not 'no weather'"
            );
        }

        assert_eq!(
            decode(&encode_entity_snapshot_with_weather_and_bare_structure(
                Some((fb::WeatherKind::Clear.0, 1)),
                None
            )),
            Err(DecodeError::ClearWeatherWithIntensity { intensity: 1 }),
            "clear weather always carries 0, and the two halves must agree"
        );
    }

    /// A campfire says whether it is burning, and a server that cannot douse one says
    /// nothing at all.
    ///
    /// The third case is the one the contract's `true` default exists for: a pre-V26
    /// server writes no byte, and the elided field has to read as the burning fire every
    /// fire on such a server is. A default of `false` would have made that same silence
    /// claim the world's fires were all out.
    ///
    /// Nothing is refused here, and that is the whole shape of this field: `lit` is a
    /// bool, so every value it can hold is a legal one, and its only rule — that it means
    /// something for a campfire and nothing for the other kinds — belongs to the reader
    /// rather than to the decoder.
    #[test]
    fn a_campfire_says_whether_it_is_burning_and_an_older_server_says_nothing() {
        let mut campfire = StructureStateWire::tent(77, 4);
        campfire.kind = fb::StructureKind::Campfire;

        for lit in [true, false] {
            let Ok(Message::Snapshot(snapshot)) = decode(&encode_entity_snapshot_with(
                1,
                &[],
                &[],
                &[],
                PlayerVitalsWire::default(),
                &[StructureStateWire { lit, ..campfire }],
            )) else {
                panic!("a campfire with lit = {lit} did not survive the wire");
            };
            assert_eq!(snapshot.structures.len(), 1);
            assert_eq!(snapshot.structures[0].lit, lit);
        }

        // Every other kind carries `true` as well, and on both of these it is the
        // contract's default showing through rather than a claim that a tent is on fire.
        for structure in [campfire, StructureStateWire::tent(78, 4)] {
            let Ok(Message::Snapshot(snapshot)) = decode(
                &encode_entity_snapshot_with_weather_and_bare_structure(None, Some(structure)),
            ) else {
                panic!("a structure with no lit byte is an ordinary structure");
            };
            assert!(
                snapshot.structures[0].lit,
                "the contract's default is true, so a pre-V26 server's silence is a burning fire"
            );
        }
    }

    /// The two client requests carry intent and nothing else.
    ///
    /// **The strongest statement about "no price and no total" is a negative one**: the
    /// generated `TradeRequestArgs` has no field to put one in, so the guarantee lives in
    /// the schema and in this round trip's field list rather than in an assertion. What is
    /// checked here is that every field survives, both directions of `buying` included —
    /// `false` is a sale, not an absent field standing in for one.
    #[test]
    fn the_settlement_requests_carry_intent_and_round_trip_every_field() {
        let interact = NpcInteractRequest {
            entity_id: 900,
            client_tick: 21,
        };
        let frame = encode_npc_interact_request(&interact);
        assert_eq!(
            decode(&frame),
            Ok(Message::ClientOnly("NpcInteractRequest")),
            "a client's own request must be refused by direction, not decoded"
        );
        let envelope = fb::root_as_envelope(&frame).expect("the encoder produced a valid frame");
        assert_eq!(envelope.payload_type(), fb::Payload::NpcInteractRequest);
        let payload = envelope
            .payload_as_npc_interact_request()
            .expect("the union tag names the payload it carries");
        assert_eq!(payload.entity_id(), interact.entity_id);
        assert_eq!(payload.client_tick(), interact.client_tick);

        for trade in [
            TradeRequest {
                entity_id: 900,
                item_id: 31,
                count: 4,
                buying: true,
                revision: 2,
                client_tick: 22,
            },
            TradeRequest {
                entity_id: 900,
                item_id: 8,
                count: 1,
                buying: false,
                revision: 2,
                client_tick: 23,
            },
        ] {
            let frame = encode_trade_request(&trade);
            assert_eq!(decode(&frame), Ok(Message::ClientOnly("TradeRequest")));
            let envelope =
                fb::root_as_envelope(&frame).expect("the encoder produced a valid frame");
            let payload = envelope
                .payload_as_trade_request()
                .expect("the union tag names the payload it carries");
            assert_eq!(
                (
                    payload.entity_id(),
                    payload.item_id(),
                    payload.count(),
                    payload.buying(),
                    payload.revision(),
                    payload.client_tick(),
                ),
                (
                    trade.entity_id,
                    trade.item_id,
                    trade.count,
                    trade.buying,
                    trade.revision,
                    trade.client_tick,
                )
            );
        }
    }

    /// Every action shares one table; the encoder rewrites none of its fields.
    #[test]
    fn player_trade_requests_round_trip_every_action() {
        for (index, action) in [
            PlayerTradeAction::Open,
            PlayerTradeAction::SetItem,
            PlayerTradeAction::ClearItem,
            PlayerTradeAction::SetSilver,
            PlayerTradeAction::Confirm,
            PlayerTradeAction::Cancel,
        ]
        .into_iter()
        .enumerate()
        {
            let request = PlayerTradeRequest {
                action,
                target_entity_id: 900 + index as u64,
                trade_slot: index as u8,
                pack_slot: 10 + index as u8,
                silver: 100 + index as u32,
                revision: 20 + index as u32,
                client_tick: 30 + index as u32,
            };
            let frame = encode_player_trade_request(&request);
            assert_eq!(
                decode(&frame),
                Ok(Message::ClientOnly("PlayerTradeRequest"))
            );
            let envelope =
                fb::root_as_envelope(&frame).expect("the encoder produced a valid frame");
            let payload = envelope
                .payload_as_player_trade_request()
                .expect("the union tag names the payload it carries");
            assert_eq!(payload.action(), action.wire());
            assert_eq!(
                (
                    payload.target_entity_id(),
                    payload.trade_slot(),
                    payload.pack_slot(),
                    payload.silver(),
                    payload.revision(),
                    payload.client_tick(),
                ),
                (
                    request.target_entity_id,
                    request.trade_slot,
                    request.pack_slot,
                    request.silver,
                    request.revision,
                    request.client_tick,
                )
            );
        }
    }

    /// The three outbound map payloads carry intent and nothing else.
    ///
    /// The request encoder writes a misaligned origin exactly as given, deliberately: it
    /// is an encoder, not a validator, and a client that corrected its own request here
    /// would hide the bug that produced it from the only boundary that checks.
    #[test]
    fn the_map_requests_carry_intent_and_are_never_corrected_on_the_way_out() {
        let frame = encode_map_tile_request(&MapTileRequest {
            origin_x: 1024,
            origin_z: -2048,
            scale: 16,
            client_tick: 9,
        });
        assert_eq!(decode(&frame), Ok(Message::ClientOnly("MapTileRequest")));
        let envelope = fb::root_as_envelope(&frame).expect("a frame this client built");
        let request = envelope
            .payload_as_map_tile_request()
            .expect("the tag names the payload");
        assert_eq!(
            (
                request.origin_x(),
                request.origin_z(),
                request.scale(),
                request.client_tick()
            ),
            (1024, -2048, 16, 9)
        );

        let off_grid = encode_map_tile_request(&MapTileRequest {
            origin_x: 1,
            origin_z: 0,
            scale: 1,
            client_tick: 0,
        });
        let envelope = fb::root_as_envelope(&off_grid).expect("a frame this client built");
        assert_eq!(
            envelope
                .payload_as_map_tile_request()
                .expect("the tag names the payload")
                .origin_x(),
            1,
            "the encoder writes what it was given; the grid is the decoder's to hold"
        );

        let place = encode_marker_place_request(&MarkerPlaceRequest {
            x: -900,
            z: 1200,
            kind: MarkerKind::Cave,
            note: "cold air".to_owned(),
            client_tick: 11,
        });
        assert_eq!(
            decode(&place),
            Ok(Message::ClientOnly("MarkerPlaceRequest"))
        );
        let envelope = fb::root_as_envelope(&place).expect("a frame this client built");
        let request = envelope
            .payload_as_marker_place_request()
            .expect("the tag names the payload");
        assert_eq!((request.x(), request.z()), (-900, 1200));
        assert_eq!(request.kind(), fb::MarkerKind::Cave);
        assert_eq!(request.note(), Some("cold air"));
        assert_eq!(request.client_tick(), 11);

        let remove = encode_marker_remove_request(&MarkerRemoveRequest {
            marker_id: 12345,
            client_tick: 13,
        });
        assert_eq!(
            decode(&remove),
            Ok(Message::ClientOnly("MarkerRemoveRequest"))
        );
        let envelope = fb::root_as_envelope(&remove).expect("a frame this client built");
        let request = envelope
            .payload_as_marker_remove_request()
            .expect("the tag names the payload");
        assert_eq!((request.marker_id(), request.client_tick()), (12345, 13));
    }

    #[test]
    fn the_refusal_reasons_keep_their_two_groups() {
        assert_eq!(fb::RefusedAction::Unknown.0, 0);
        assert_eq!(fb::RefusedAction::PlaceStructure.0, 1);
        assert_eq!(fb::RefusedAction::MineBlock.0, 2);
        assert_eq!(fb::RefusedAction::EditBlock.0, 3);
        assert_eq!(fb::RefusedAction::Craft.0, 4);
        assert_eq!(fb::RefusedAction::Repair.0, 5);
        assert_eq!(fb::RefusedAction::DropItem.0, 6);
        assert_eq!(fb::RefusedAction::Chat.0, 7);
        assert_eq!(fb::RefusedAction::Party.0, 8);
        assert_eq!(fb::RefusedAction::OpenLoot.0, 9);
        assert_eq!(fb::RefusedAction::TakeLoot.0, 10);
        assert_eq!(fb::RefusedAction::Attack.0, 11);
        // V24 reserves the map's three, on the same terms as the members above: the
        // number is free now and will not be once anything depends on it.
        assert_eq!(fb::RefusedAction::RequestMapTile.0, 12);
        assert_eq!(fb::RefusedAction::PlaceMarker.0, 13);
        assert_eq!(fb::RefusedAction::RemoveMarker.0, 14);
        // V25 reserves the settlement's two, on the same terms.
        assert_eq!(fb::RefusedAction::Interact.0, 15);
        assert_eq!(fb::RefusedAction::Trade.0, 16);
        // V26 reserves the two actions warded ground refuses, on the same terms. `Mine`
        // sits beside the reserved `MineBlock` = 2 rather than replacing it: removing or
        // renumbering that one would relabel every refusal a shipped server has sent.
        assert_eq!(fb::RefusedAction::Edit.0, 17);
        assert_eq!(fb::RefusedAction::Mine.0, 18);
        assert_eq!(fb::RefusedAction::Mount.0, 19);
        assert_eq!(fb::RefusedAction::PlayerTrade.0, 20);
        // No member for a removal, and its absence is the decision: a refused removal is
        // silence on purpose, because a client that could tell "no such structure" from
        // "not yours" from "too far away" could map somebody else's camp by asking.
        //
        // A drop has one for that same reason read the other way, which is why the pair is
        // worth keeping in one assertion: every question a refused drop could answer — that
        // slot is empty, that item wears out, you are dead — is about the asking player's
        // own pack, which they are already holding a complete `InventoryState` of.
        assert_eq!(
            fb::RefusedAction::ENUM_VALUES.len(),
            21,
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
            (fb::RefusalReason::TooFast, 12),
            (fb::RefusalReason::PartyFull, 13),
            (fb::RefusalReason::NoSuchPlayer, 14),
            (fb::RefusalReason::AlreadyInParty, 15),
            (fb::RefusalReason::NoInvite, 16),
            (fb::RefusalReason::NotLeader, 17),
            (fb::RefusalReason::CorpseUnavailable, 18),
            (fb::RefusalReason::LootNotOwned, 19),
            (fb::RefusalReason::StaleRevision, 20),
            (fb::RefusalReason::InventoryFull, 21),
            (fb::RefusalReason::NoAmmunition, 22),
            (fb::RefusalReason::TileMisaligned, 23),
            (fb::RefusalReason::TooManyMarkers, 24),
            (fb::RefusalReason::NoteTooLong, 25),
            (fb::RefusalReason::MarkerUnknown, 26),
            // V25's three, appended inside the low group: each is the world answering a
            // legal question no.
            (fb::RefusalReason::NotAVendor, 27),
            (fb::RefusalReason::NotEnoughSilver, 28),
            (fb::RefusalReason::VendorDoesNotWant, 29),
            // V26's one, appended inside the low group: warded ground is the world
            // answering a legal question no, and the player can walk somewhere else.
            (fb::RefusalReason::Warded, 30),
            (fb::RefusalReason::MountNotLearned, 31),
            (fb::RefusalReason::AlreadyMounted, 32),
            (fb::RefusalReason::MountNotGrounded, 33),
            (fb::RefusalReason::MountIndoors, 34),
            (fb::RefusalReason::MountLowCeiling, 35),
            (fb::RefusalReason::CastAlreadyInProgress, 36),
            (fb::RefusalReason::CastInterruptedByDamage, 37),
            (fb::RefusalReason::CastInterruptedByMovement, 38),
            (fb::RefusalReason::CastInterruptedByJump, 39),
            (fb::RefusalReason::CastInterruptedByDeath, 40),
            (fb::RefusalReason::ActionForbiddenWhileMounted, 41),
            (fb::RefusalReason::MountAlreadyLearned, 42),
            (fb::RefusalReason::AlreadyTrading, 43),
            (fb::RefusalReason::TradeNotOpen, 44),
            (fb::RefusalReason::TradeSlotTaken, 45),
            (fb::RefusalReason::NothingToOffer, 46),
            (fb::RefusalReason::TradeCooldown, 47),
            (fb::RefusalReason::MalformedNoAnchor, 64),
            (fb::RefusalReason::MalformedFacing, 65),
            (fb::RefusalReason::MalformedSlot, 66),
            (fb::RefusalReason::MalformedKind, 67),
        ] {
            assert_eq!(reason.0, value, "{reason:?}");
        }
        assert_eq!(
            fb::RefusalReason::ENUM_VALUES.len(),
            52,
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
            RefusalReason::TooFast,
            RefusalReason::PartyFull,
            RefusalReason::NoSuchPlayer,
            RefusalReason::AlreadyInParty,
            RefusalReason::NoInvite,
            RefusalReason::NotLeader,
            RefusalReason::CorpseUnavailable,
            RefusalReason::LootNotOwned,
            RefusalReason::StaleRevision,
            RefusalReason::InventoryFull,
            RefusalReason::TileMisaligned,
            RefusalReason::TooManyMarkers,
            RefusalReason::NoteTooLong,
            RefusalReason::MarkerUnknown,
            RefusalReason::NotAVendor,
            RefusalReason::NotEnoughSilver,
            RefusalReason::VendorDoesNotWant,
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
        assert_eq!(fb::MobKind::Vargr.0, 2);
        assert_eq!(fb::MobKind::Deer.0, 3);
        assert_eq!(fb::MobKind::Villager.0, 4);
        assert_eq!(fb::MobKind::Horse.0, 5);

        assert_eq!(fb::MobAction::Unknown.0, 0);
        assert_eq!(fb::MobAction::Idle.0, 1);
        assert_eq!(fb::MobAction::Chase.0, 2);
        assert_eq!(fb::MobAction::Windup.0, 3);
        assert_eq!(fb::MobAction::Recovery.0, 4);
        // Appended by V9, pinned with the four before it: the value is an integer on the
        // wire, so a renumbering would draw one action where the server said another.
        assert_eq!(fb::MobAction::Dying.0, 5);
        assert_eq!(fb::MobAction::Flee.0, 6);
        assert_eq!(fb::MobAction::Corpse.0, 7);
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
        assert_eq!(fb::RecipeID::CookedMeat.0, 10);
        assert_eq!(fb::RecipeID::LeatherCap.0, 11);
        assert_eq!(fb::RecipeID::LeatherJerkin.0, 12);
        assert_eq!(fb::RecipeID::LeatherLeggings.0, 13);
        assert_eq!(fb::RecipeID::IronHelm.0, 14);
        assert_eq!(fb::RecipeID::IronCuirass.0, 15);
        assert_eq!(fb::RecipeID::IronGreaves.0, 16);
        assert_eq!(fb::RecipeID::WoodenShield.0, 17);
        assert_eq!(fb::RecipeID::Bow.0, 18);
        assert_eq!(fb::RecipeID::Arrows.0, 19);
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
        // 4 was the first value past the end until V25 named it `Villager`, and 5 until
        // V27 reserved `Horse`. The test moved to 6 rather than being deleted, because
        // what it pins is "one past the contract", not the literal.
        assert_eq!(
            MobKind::from_wire(fb::MobKind::Villager),
            Some(MobKind::Villager)
        );
        // V27 reserved Horse together with the mount contract. It is accepted now that
        // the paddock renderer exists; the wire member and version do not move.
        assert_eq!(MobKind::from_wire(fb::MobKind::Horse), Some(MobKind::Horse));
        assert_eq!(MobKind::from_wire(fb::MobKind(6)), None);
        assert_eq!(MobKind::from_wire(fb::MobKind(200)), None);

        assert_eq!(StructureKind::from_wire(fb::StructureKind::Unknown), None);
        // V26's runestone is accepted in the same commit that gives it a structure mesh.
        assert_eq!(fb::StructureKind::Runestone.0, 4);
        assert_eq!(
            StructureKind::from_wire(fb::StructureKind::Runestone),
            Some(StructureKind::Runestone)
        );
        assert_eq!(StructureKind::from_wire(fb::StructureKind(5)), None);
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

    /// A welcome that says nothing about voice announces no voice at all.
    ///
    /// An absent scalar decodes as zero, which is the pre-V30 shape *and* the legal
    /// answer of a V30 operator who turned voice off — so this is the same test as the
    /// clock's above and it is here for the same reason: the older peer stays readable
    /// and the zero is a value rather than a gap.
    #[test]
    fn a_welcome_that_says_nothing_about_voice_relays_none() {
        let Ok(Message::Welcome(params)) = decode_welcome(&WelcomeWire::default()) else {
            panic!("a default welcome is accepted");
        };

        assert_eq!(params.voice_range_blocks, 0.0);
    }

    /// A declared voice range arrives unchanged, and a nonsensical one is refused.
    ///
    /// Zero is in the accepted column deliberately: it is an announcement, not a
    /// degenerate radius. The two non-finite cases are the ones a clamp would pass
    /// through — `NaN` compares false against every bound it is given — which is why
    /// this decoder refuses rather than repairs, exactly as it does for a spawn axis.
    #[test]
    fn a_voice_range_is_finite_and_not_negative_or_the_welcome_is_refused() {
        for blocks in [0.0, 0.5, 24.0, f32::MAX] {
            let Ok(Message::Welcome(params)) = decode_welcome(&WelcomeWire {
                voice_range_blocks: blocks,
                ..WelcomeWire::default()
            }) else {
                panic!("{blocks} blocks is a range a server may announce");
            };
            assert_eq!(params.voice_range_blocks, blocks);
        }

        for blocks in [-1.0, f32::NEG_INFINITY, f32::INFINITY] {
            assert_eq!(
                decode_welcome(&WelcomeWire {
                    voice_range_blocks: blocks,
                    ..WelcomeWire::default()
                }),
                Err(DecodeError::VoiceRange(blocks)),
                "{blocks} blocks is not a range"
            );
        }

        // `NaN` is its own case, because it is not equal to itself and so cannot be
        // compared with the value the error carries.
        let refused = decode_welcome(&WelcomeWire {
            voice_range_blocks: f32::NAN,
            ..WelcomeWire::default()
        });
        assert!(
            matches!(refused, Err(DecodeError::VoiceRange(got)) if got.is_nan()),
            "NaN blocks is not a range, got {refused:?}"
        );
    }

    /// A relayed frame arrives with its speaker, its counter and its bytes unchanged.
    ///
    /// The bytes are compared and never interpreted: whether they are a legal Opus frame
    /// is the decoder's question in #851, and this layer's job is to hand over exactly
    /// what the server relayed.
    #[test]
    fn a_relayed_voice_frame_carries_its_speaker_and_its_bytes() {
        let opus = [0x78_u8, 0x00, 0xff, 0x11];
        assert_eq!(
            decode(&server_side::encode_voice_heard(41, 7, Some(&opus))),
            Ok(Message::VoiceHeard(VoiceHeard {
                speaker_entity_id: 41,
                sequence: 7,
                opus: opus.to_vec(),
            }))
        );
    }

    /// The three shapes a relayed frame may not have.
    ///
    /// Absent and empty `opus` are one nothing with two spellings and owe the same
    /// answer; the ceiling is the contract's own `VoiceBound`, and the frame one byte
    /// past it is refused rather than truncated, because this side allocates from a
    /// length the peer chose. The frame exactly at the bound is accepted in the same
    /// breath, since a bound tested only from the refusing side is a bound that may be
    /// off by one.
    #[test]
    fn a_voice_frame_with_no_speaker_no_audio_or_too_much_of_it_is_refused() {
        assert_eq!(
            decode(&server_side::encode_voice_heard(0, 1, Some(&[9]))),
            Err(DecodeError::VoiceWithoutSpeaker)
        );

        for opus in [None, Some(&[][..])] {
            assert_eq!(
                decode(&server_side::encode_voice_heard(41, 1, opus)),
                Err(DecodeError::VoiceWithoutAudio {
                    speaker_entity_id: 41
                }),
                "{opus:?} is a frame with no audio in it"
            );
        }

        assert_eq!(MAX_OPUS_BYTES, 400);
        let at_the_bound = vec![7_u8; MAX_OPUS_BYTES];
        let Ok(Message::VoiceHeard(heard)) =
            decode(&server_side::encode_voice_heard(41, 1, Some(&at_the_bound)))
        else {
            panic!("a frame of exactly MAX_OPUS_BYTES is one the server may relay");
        };
        assert_eq!(heard.opus.len(), MAX_OPUS_BYTES);

        let over = vec![7_u8; MAX_OPUS_BYTES + 1];
        assert_eq!(
            decode(&server_side::encode_voice_heard(41, 1, Some(&over))),
            Err(DecodeError::OversizedVoiceFrame {
                speaker_entity_id: 41,
                len: MAX_OPUS_BYTES + 1,
            })
        );
    }

    /// No refusal this payload can produce carries the audio it refused.
    ///
    /// The contract says a voice frame is never logged, never persisted and never quoted
    /// in a diagnostic, and a `DecodeError` is a diagnostic: `session.rs` turns every one
    /// of them into a status line and a log line. The bytes here are chosen so that a
    /// decoder which printed them would be caught — each is a decimal run no id or length
    /// in the message could produce on its own.
    #[test]
    fn a_voice_refusal_never_prints_the_bytes_it_refused() {
        let over = vec![0xAB_u8; MAX_OPUS_BYTES + 1];
        for frame in [
            server_side::encode_voice_heard(0, 1, Some(&over)),
            server_side::encode_voice_heard(41, 1, Some(&over)),
        ] {
            let rendered = decode(&frame)
                .expect_err("both frames are refused")
                .to_string();
            assert!(
                !rendered.contains("171") && !rendered.contains("0xAB"),
                "a voice refusal quoted its payload: {rendered}"
            );
        }
    }

    /// The two world-clock projections ride in the snapshot and arrive unchanged, the
    /// last tick of a day included.
    ///
    /// Nothing here checks that bound, and that is the design: this layer decodes one
    /// frame and the day length arrived in another. `net::handshake` owns the check.
    #[test]
    fn a_snapshot_carries_the_absolute_and_wrapped_world_clock() {
        for tick_of_day in [0_u32, 1, 14_400, 23_999] {
            let world_tick = 12 * 24_000 + u64::from(tick_of_day);
            let frame =
                server_side::encode_entity_snapshot_at_world_tick(7, tick_of_day, world_tick);
            let Ok(Message::Snapshot(snapshot)) = decode(&frame) else {
                panic!("a snapshot with a tick of day is a snapshot");
            };

            assert_eq!(snapshot.tick_of_day, tick_of_day);
            assert_eq!(snapshot.world_tick, world_tick);
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
    fn a_player_appearance_decodes_into_its_entity_face_name_level_and_worn_items() {
        let decoded = decode(&encode_player_appearance_with_worn(
            4242,
            Some(AppearanceWire::default()),
            Some("Brynhildr"),
            12,
            [101, 102, 103, 104],
        ));

        assert_eq!(
            decoded,
            Ok(Message::PlayerAppearance(PlayerAppearance {
                entity_id: 4242,
                appearance: an_appearance(),
                name: "Brynhildr".to_owned(),
                level: 12,
                worn_head: 101,
                worn_chest: 102,
                worn_legs: 103,
                worn_offhand: 104,
            }))
        );
    }

    #[test]
    fn an_empty_or_unbounded_unicode_name_is_still_display_text() {
        let long = "ᚠe\u{301}".repeat(2_048);
        for name in ["", long.as_str()] {
            let decoded = decode(&encode_player_appearance(
                7,
                Some(AppearanceWire::default()),
                Some(name),
                1,
            ));
            let Ok(Message::PlayerAppearance(described)) = decoded else {
                panic!("the display text was refused: {decoded:?}");
            };
            assert_eq!(described.name, name);
        }
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
            Some("Unseen"),
            1,
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
                Some(AppearanceWire::default()),
                Some("Nobody"),
                1,
            )),
            Err(DecodeError::AppearanceWithoutEntity)
        );
        assert_eq!(
            decode(&encode_player_appearance(1, None, Some("No face"), 1)),
            Err(DecodeError::MissingAppearance {
                at: "PlayerAppearance"
            })
        );
        assert_eq!(
            decode(&encode_player_appearance(
                1,
                Some(AppearanceWire::default()),
                None,
                1,
            )),
            Err(DecodeError::AppearanceWithoutName(1))
        );
        // FlatBuffers reads an absent scalar as its default zero, so a V16 frame and a
        // V17 builder handed zero are the same invalid value at this boundary.
        for frame in [
            encode_player_appearance_without_level(
                1,
                Some(AppearanceWire::default()),
                Some("No level"),
            ),
            encode_player_appearance(1, Some(AppearanceWire::default()), Some("Zero level"), 0),
        ] {
            assert_eq!(decode(&frame), Err(DecodeError::AppearanceWithoutLevel(1)));
        }
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
            match decode(&encode_player_appearance(1, Some(wire), Some("Colour"), 1)) {
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
                decode(&encode_player_appearance(1, Some(wire), Some("Hair"), 1)),
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
            decode(&encode_player_appearance(
                1,
                Some(wire),
                Some("Placeholder"),
                1,
            )),
            Ok(Message::PlayerAppearance(PlayerAppearance {
                entity_id: 1,
                appearance: PLACEHOLDER_APPEARANCE,
                name: "Placeholder".to_owned(),
                level: 1,
                worn_head: 0,
                worn_chest: 0,
                worn_legs: 0,
                worn_offhand: 0,
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
            encode_player_appearance(1, Some(AppearanceWire::default()), Some("Corruption"), 1),
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
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: Some(vec![7; PLAYER_TOKEN_LEN]),
            clock: WorldClock {
                day_length_ticks: 24_000,
                night_start_ticks: 14_400,
                night_end_ticks: 21_600,
            },
            voice_range_blocks: 24.0,
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
                inventory_slots: 37,
                hotbar_slots: 9,
                equipment_slots: 4,
                player_token: PlayerToken::from_bytes([7; PLAYER_TOKEN_LEN]),
                clock: WorldClock {
                    day_length_ticks: 24_000,
                    night_start_ticks: 14_400,
                    night_end_ticks: 21_600,
                },
                voice_range_blocks: 24.0,
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
    fn zero_inventory_hotbar_and_equipment_slot_counts_are_protocol_errors() {
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
        assert_eq!(
            decode_welcome(&WelcomeWire {
                equipment_slots: 0,
                ..WelcomeWire::default()
            }),
            Err(DecodeError::EquipmentSlots(0))
        );
    }

    #[test]
    fn more_than_eight_equipment_slots_is_a_protocol_error() {
        assert_eq!(
            decode_welcome(&WelcomeWire {
                equipment_slots: MAX_EQUIPMENT_SLOTS + 1,
                ..WelcomeWire::default()
            }),
            Err(DecodeError::EquipmentSlots(MAX_EQUIPMENT_SLOTS + 1))
        );
    }

    #[test]
    fn hotbar_and_equipment_larger_than_the_inventory_is_a_protocol_error() {
        assert_eq!(
            decode_welcome(&WelcomeWire {
                inventory_slots: 12,
                hotbar_slots: 9,
                equipment_slots: 4,
                ..WelcomeWire::default()
            }),
            Err(DecodeError::ReservedSlotsExceedInventory {
                hotbar: 9,
                equipment: 4,
                inventory: 12,
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
                    world_tick: 0,
                    server_tick: 5,
                    entities: Vec::new(),
                    drops: Vec::new(),
                    projectiles: Vec::new(),
                    mobs: vec![],
                    self_vitals: PlayerVitals::unharmed(),
                    structures: vec![],
                    dead_players: vec![],
                    blocking_players: vec![],
                    party_leader_entity_id: 0,
                    party_members: vec![],
                    party_roster: vec![],
                    accessible_loot_corpses: vec![],
                    weather: None,
                    mounts: vec![],
                    self_cast: None,
                })),
                "{name}"
            );
        }
    }

    #[test]
    fn mount_projection_and_recipient_cast_decode_in_wire_order() {
        let entities = [EntityStateWire::at(7, 0.0), EntityStateWire::at(9, 4.0)];
        let frame = encode_snapshot_with_mounts(
            &entities,
            &[
                (9, fb::MountKind::GreyHorse),
                (7, fb::MountKind::BlackHorse),
            ],
            Some((fb::CastKind::Mount, 128)),
        );
        let Ok(Message::Snapshot(snapshot)) = decode(&frame) else {
            panic!("a valid V27 snapshot was refused");
        };
        assert_eq!(
            snapshot.mounts,
            vec![
                MountState {
                    entity_id: 9,
                    mount: MountKind::GreyHorse,
                },
                MountState {
                    entity_id: 7,
                    mount: MountKind::BlackHorse,
                },
            ]
        );
        assert_eq!(
            snapshot.self_cast,
            Some(CastState {
                kind: CastKind::Mount,
                progress: 128,
            })
        );
    }

    #[test]
    fn mount_projection_refuses_missing_duplicate_and_unknown_players() {
        let entities = [EntityStateWire::at(7, 0.0)];
        assert_eq!(
            decode(&encode_snapshot_with_mounts(
                &[EntityStateWire::at(0, 0.0)],
                &[],
                None,
            )),
            Err(DecodeError::EntityWithoutIdentity)
        );
        assert_eq!(
            decode(&encode_snapshot_with_mounts(
                &[EntityStateWire::at(7, 0.0), EntityStateWire::at(7, 4.0)],
                &[],
                None,
            )),
            Err(DecodeError::DuplicateEntity(7))
        );
        assert_eq!(
            decode(&encode_snapshot_with_mounts(
                &entities,
                &[(0, fb::MountKind::BlackHorse)],
                None,
            )),
            Err(DecodeError::MountNotInSnapshot(0))
        );
        assert_eq!(
            decode(&encode_snapshot_with_mounts(
                &entities,
                &[(9, fb::MountKind::BlackHorse)],
                None,
            )),
            Err(DecodeError::MountNotInSnapshot(9))
        );
        assert_eq!(
            decode(&encode_snapshot_with_mounts(
                &entities,
                &[
                    (7, fb::MountKind::BlackHorse),
                    (7, fb::MountKind::BrownHorse),
                ],
                None,
            )),
            Err(DecodeError::DuplicateMountState(7))
        );
        assert_eq!(
            decode(&encode_snapshot_with_mounts(
                &entities,
                &[(7, fb::MountKind(99))],
                None,
            )),
            Err(DecodeError::UnknownMountKind {
                entity_id: 7,
                value: 99,
            })
        );
    }

    #[test]
    fn recipient_cast_refuses_unknown_kind_and_completed_progress() {
        assert_eq!(
            decode(&encode_snapshot_with_mounts(
                &[],
                &[],
                Some((fb::CastKind::Unknown, 0)),
            )),
            Err(DecodeError::UnknownCastKind(0))
        );
        assert_eq!(
            decode(&encode_snapshot_with_mounts(
                &[],
                &[],
                Some((fb::CastKind::Mount, u8::MAX)),
            )),
            Err(DecodeError::CompletedCast)
        );
    }

    #[test]
    fn learned_mounts_is_a_complete_known_unique_set() {
        assert_eq!(
            decode(&encode_learned_mounts(Some(&[
                fb::MountKind::GreyHorse,
                fb::MountKind::BlackHorse,
            ]))),
            Ok(Message::LearnedMounts(LearnedMounts {
                mounts: vec![MountKind::GreyHorse, MountKind::BlackHorse],
            }))
        );
        for frame in [
            encode_learned_mounts(None),
            encode_learned_mounts(Some(&[])),
        ] {
            assert_eq!(
                decode(&frame),
                Ok(Message::LearnedMounts(LearnedMounts { mounts: vec![] }))
            );
        }
        assert_eq!(
            decode(&encode_learned_mounts(Some(&[fb::MountKind::Unknown]))),
            Err(DecodeError::UnknownLearnedMount(0))
        );
        assert_eq!(
            decode(&encode_learned_mounts(Some(&[
                fb::MountKind::BrownHorse,
                fb::MountKind::BrownHorse,
            ]))),
            Err(DecodeError::DuplicateLearnedMount(MountKind::BrownHorse))
        );
    }

    #[test]
    fn mount_and_dismount_encoders_are_intent_only() {
        let mount = encode_mount_request(&MountRequest {
            mount: MountKind::BrownHorse,
        });
        assert_eq!(decode(&mount), Ok(Message::ClientOnly("MountRequest")));
        let envelope = fb::root_as_envelope(&mount).expect("a frame this client built");
        assert_eq!(
            envelope
                .payload_as_mount_request()
                .expect("the tag names the payload")
                .mount(),
            fb::MountKind::BrownHorse
        );

        let dismount = encode_dismount_request();
        assert_eq!(
            decode(&dismount),
            Ok(Message::ClientOnly("DismountRequest"))
        );
    }

    /// The dead arrive in the order the server gave them, and an empty vector is the ordinary
    /// frame rather than a special one.
    #[test]
    fn a_snapshot_decodes_the_players_the_server_holds_dead() {
        let entities = [EntityStateWire::at(7, 0.0), EntityStateWire::at(9, 4.0)];
        let dead_vitals = PlayerVitalsWire {
            health: 0,
            life_state: fb::LifeState::Dead,
            respawn_ticks: 40,
            ..Default::default()
        };

        for (vitals, want) in [
            (dead_vitals, vec![9, 7]),
            (PlayerVitalsWire::default(), vec![]),
        ] {
            let frame = encode_entity_snapshot_with_dead(3, &entities, vitals, &want);
            let Ok(Message::Snapshot(snapshot)) = decode(&frame) else {
                panic!("a snapshot naming {want:?} did not decode");
            };
            assert_eq!(snapshot.dead_players, want);
        }
    }

    /// **A dead player nobody can see is a frame this client refuses**, because
    /// `dead_players` describes the bodies in *this* snapshot: an id outside `entities` has
    /// nowhere to go, and remembering it would be this client keeping a fact the next
    /// snapshot is entitled to contradict. A repeated id is refused for the reason the
    /// entity-id conflicts above are: one id names one thing.
    #[test]
    fn a_dead_player_who_is_not_in_the_snapshot_is_a_protocol_error() {
        let entities = [EntityStateWire::at(7, 0.0)];
        let frame = |dead: &[u64]| {
            encode_entity_snapshot_with_dead(1, &entities, PlayerVitalsWire::default(), dead)
        };

        assert_eq!(
            decode(&frame(&[8])),
            Err(DecodeError::DeadPlayerNotInSnapshot(8))
        );
        assert_eq!(
            decode(&frame(&[7, 7])),
            Err(DecodeError::DeadPlayerNamedTwice(7))
        );
    }

    #[test]
    fn a_snapshot_decodes_projectiles_in_wire_order() {
        let projectiles = [
            ProjectileStateWire::arrow(40, -1.0),
            ProjectileStateWire {
                entity_id: 41,
                pos: [3.0, 70.5, 2.0],
                vel: [-4.0, 1.5, 0.25],
                kind: fb::ProjectileKind::EnergyOrb,
            },
        ];

        let Ok(Message::Snapshot(snapshot)) =
            decode(&encode_entity_snapshot_with_projectiles(88, &projectiles))
        else {
            panic!("the projectile snapshot did not decode");
        };
        assert_eq!(
            snapshot.projectiles,
            vec![
                ProjectileState {
                    entity_id: 40,
                    pos: [-1.0, 64.0, -2.0],
                    vel: [28.0, -3.0, 0.0],
                    kind: ProjectileKind::Arrow,
                },
                ProjectileState {
                    entity_id: 41,
                    pos: [3.0, 70.5, 2.0],
                    vel: [-4.0, 1.5, 0.25],
                    kind: ProjectileKind::EnergyOrb,
                },
            ]
        );
    }

    #[test]
    fn a_snapshot_refuses_malformed_projectiles() {
        let mut non_finite = ProjectileStateWire::arrow(40, 0.0);
        non_finite.vel[1] = f32::NAN;
        assert!(matches!(
            decode(&encode_entity_snapshot_with_projectiles(1, &[non_finite])),
            Err(DecodeError::NonFiniteProjectile {
                entity_id: 40,
                field: "vel.y",
                value,
            }) if value.is_nan()
        ));

        let unknown = ProjectileStateWire {
            kind: fb::ProjectileKind(99),
            ..ProjectileStateWire::arrow(41, 0.0)
        };
        assert_eq!(
            decode(&encode_entity_snapshot_with_projectiles(1, &[unknown])),
            Err(DecodeError::UnknownProjectileKind {
                entity_id: 41,
                value: 99,
            })
        );

        let duplicate = ProjectileStateWire::arrow(42, 0.0);
        assert_eq!(
            decode(&encode_entity_snapshot_with_projectiles(
                1,
                &[duplicate, duplicate]
            )),
            Err(DecodeError::DuplicateProjectile(42))
        );
    }

    #[test]
    fn raised_shield_players_decode_and_must_name_visible_players_once() {
        let entities = [EntityStateWire::at(7, 0.0)];
        let raised = PlayerVitalsWire {
            blocking: true,
            ..Default::default()
        };
        let Ok(Message::Snapshot(snapshot)) = decode(
            &server_side::encode_entity_snapshot_with_blocking(1, &entities, raised, &[7]),
        ) else {
            panic!("a valid raised-shield snapshot did not decode");
        };
        assert!(snapshot.self_vitals.blocking);
        assert_eq!(snapshot.blocking_players, vec![7]);

        assert_eq!(
            decode(&server_side::encode_entity_snapshot_with_blocking(
                1,
                &entities,
                raised,
                &[8],
            )),
            Err(DecodeError::BlockingPlayerNotInSnapshot(8))
        );
        assert_eq!(
            decode(&server_side::encode_entity_snapshot_with_blocking(
                1,
                &entities,
                raised,
                &[7, 7],
            )),
            Err(DecodeError::BlockingPlayerNamedTwice(7))
        );
    }

    #[test]
    fn a_snapshot_decodes_item_drops_in_wire_order() {
        let drops = [
            ItemDropStateWire {
                entity_id: 40,
                pos: [-1.0, 70.5, 3.0],
                item_id: 2,
                count: 7,
                durability: 0,
                max_durability: 0,
            },
            ItemDropStateWire {
                entity_id: 41,
                pos: [8.25, 12.0, -9.5],
                item_id: u16::MAX,
                count: 1,
                durability: 12,
                max_durability: 200,
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
                        durability: 0,
                        max_durability: 0,
                    },
                    ItemDropState {
                        entity_id: 41,
                        pos: [8.25, 12.0, -9.5],
                        item_id: u16::MAX,
                        count: 1,
                        durability: 12,
                        max_durability: 200,
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
        assert_eq!(
            decode(&encode_entity_snapshot_with_drops(
                1,
                &[],
                &[
                    ItemDropStateWire::item(14, 2),
                    ItemDropStateWire::item(14, 3),
                ],
            )),
            Err(DecodeError::DuplicateDrop(14))
        );
    }

    #[test]
    fn malformed_drop_durability_associations_are_protocol_errors() {
        let drop = ItemDropStateWire::item(40, 3);
        let frame = |durabilities: &[ItemDropDurabilityWire]| {
            encode_entity_snapshot_with_drop_durabilities(1, &[drop], durabilities)
        };

        assert_eq!(
            decode(&frame(&[ItemDropDurabilityWire {
                entity_id: 41,
                durability: 12,
                max_durability: 200,
            }])),
            Err(DecodeError::DropDurabilityWithoutDrop(41))
        );
        assert_eq!(
            decode(&frame(&[
                ItemDropDurabilityWire {
                    entity_id: 40,
                    durability: 12,
                    max_durability: 200,
                },
                ItemDropDurabilityWire {
                    entity_id: 40,
                    durability: 11,
                    max_durability: 200,
                },
            ])),
            Err(DecodeError::DropDurabilityNamedTwice(40))
        );

        for (count, durability, max_durability) in [(2, 12, 200), (1, 12, 0), (1, 201, 200)] {
            let mut malformed = drop;
            malformed.count = count;
            assert_eq!(
                decode(&encode_entity_snapshot_with_drop_durabilities(
                    1,
                    &[malformed],
                    &[ItemDropDurabilityWire {
                        entity_id: 40,
                        durability,
                        max_durability,
                    }],
                )),
                Err(DecodeError::DropDurability {
                    entity_id: 40,
                    count,
                    durability,
                    max_durability,
                })
            );
        }
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
                silver: 0,
            }))
        );
    }

    #[test]
    fn silver_is_part_of_complete_inventory_and_loot_state() {
        assert_eq!(
            decode(&encode_empty_inventory_with_silver(1_234)),
            Ok(Message::Inventory(InventoryState {
                stacks: vec![],
                silver: 1_234,
            }))
        );
        assert_eq!(
            decode(&encode_empty_loot_with_silver(37)),
            Ok(Message::LootState(LootState {
                corpse_id: 7,
                revision: 1,
                entries: vec![],
                silver: 37,
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
                Ok(Message::Inventory(InventoryState {
                    stacks: vec![],
                    silver: 0
                }))
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

    #[test]
    fn a_block_request_carries_only_the_edge_and_shared_tick() {
        let frame = encode_block_request(&BlockRequest {
            active: true,
            client_tick: u32::MAX,
        });
        let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
        assert_eq!(envelope.payload_type(), fb::Payload::BlockRequest);
        let request = envelope
            .payload_as_block_request()
            .expect("the payload is a block request");
        assert!(request.active());
        assert_eq!(request.client_tick(), u32::MAX);
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
            (RecipeId::CookedMeat, fb::RecipeID::CookedMeat),
            (RecipeId::LeatherCap, fb::RecipeID::LeatherCap),
            (RecipeId::LeatherJerkin, fb::RecipeID::LeatherJerkin),
            (RecipeId::LeatherLeggings, fb::RecipeID::LeatherLeggings),
            (RecipeId::IronHelm, fb::RecipeID::IronHelm),
            (RecipeId::IronCuirass, fb::RecipeID::IronCuirass),
            (RecipeId::IronGreaves, fb::RecipeID::IronGreaves),
            (RecipeId::WoodenShield, fb::RecipeID::WoodenShield),
            (RecipeId::Bow, fb::RecipeID::Bow),
            (RecipeId::Arrows, fb::RecipeID::Arrows),
            (RecipeId::WoodenSceptre, fb::RecipeID::WoodenSceptre),
            (RecipeId::Runestone, fb::RecipeID::Runestone),
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

    // -----------------------------------------------------------------------
    // Protocol V15 — consume intent
    // -----------------------------------------------------------------------

    /// One slot and a tick, with no item id, count or restoration claim.
    ///
    /// The largest slot proves the `ushort` contract is preserved rather than narrowed
    /// to the current pack-size type. The server decides that such a slot is unusable.
    #[test]
    fn a_consume_request_carries_one_slot_and_a_tick_verbatim() {
        for (slot, client_tick) in [(0, 0), (7, 41), (u16::MAX, u32::MAX)] {
            let frame = encode_consume_request(&ConsumeRequest { slot, client_tick });
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            assert_eq!(envelope.payload_type(), fb::Payload::ConsumeRequest);
            let request = envelope
                .payload_as_consume_request()
                .expect("the payload is a consume request");
            assert_eq!(request.slot(), slot);
            assert_eq!(request.client_tick(), client_tick);
            if slot != 0 && client_tick != 0 {
                assert_eq!(
                    (request._tab.vtable().num_bytes() - 4) / 2,
                    2,
                    "ConsumeRequest carries something besides a slot and a tick"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Protocol V8 — drop intent
    // -----------------------------------------------------------------------

    /// One slot index and a tick and nothing that could be a count or a position, read back
    /// through the generated accessors for the reason a repair is: this is an outbound
    /// message and one never arrives here.
    ///
    /// The out-of-range index is in the table deliberately — `schemas/player.fbs` asks for it
    /// verbatim, so a slot past the end of the pack is an ordinary refusal in the simulation
    /// rather than something this encoder gets to clamp.
    ///
    /// The field count is the absence asserted rather than described: a vtable is a `u16`
    /// count plus a `u16` per field, and two is what says nobody added a count for how much
    /// leaves the pack or a position for where it lands. It is measured on the one case
    /// where neither field is zero, because FlatBuffers writes no bytes for a field equal to
    /// its default — the all-zero frame below has an empty vtable and always will.
    #[test]
    fn a_drop_item_request_carries_one_slot_and_a_tick_verbatim() {
        for (slot, client_tick) in [(0, 0), (7, 41), (255, u32::MAX)] {
            let frame = encode_drop_item_request(&DropItemRequest { slot, client_tick });
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            assert_eq!(envelope.payload_type(), fb::Payload::DropItemRequest);
            let request = envelope
                .payload_as_drop_item_request()
                .expect("the payload is a drop request");
            assert_eq!(request.slot(), slot);
            assert_eq!(request.client_tick(), client_tick);
            if slot != 0 && client_tick != 0 {
                assert_eq!(
                    (request._tab.vtable().num_bytes() - 4) / 2,
                    2,
                    "DropItemRequest carries something besides a slot and a tick"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Protocol V12 — authoritative leave
    // -----------------------------------------------------------------------

    #[test]
    fn a_leave_request_carries_no_client_decision() {
        let frame = encode_leave_request();
        let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
        assert_eq!(envelope.payload_type(), fb::Payload::LeaveRequest);
        let request = envelope
            .payload_as_leave_request()
            .expect("the payload is a leave request");
        assert_eq!(
            (request._tab.vtable().num_bytes() - 4) / 2,
            0,
            "LeaveRequest must not carry a duration, cancellation or client outcome"
        );
    }

    #[test]
    fn a_leave_started_carries_the_server_s_remaining_time() {
        assert_eq!(
            decode(&encode_leave_started(10_000)),
            Ok(Message::LeaveStarted(LeaveStarted {
                remaining_ms: 10_000,
            }))
        );
        assert_eq!(
            decode(&encode_leave_started(0)),
            Err(DecodeError::LeaveWithoutTime)
        );
    }

    #[test]
    fn leave_cancellation_carries_only_a_request_and_the_server_s_answer() {
        let frame = encode_leave_cancel_request();
        let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
        assert_eq!(envelope.payload_type(), fb::Payload::LeaveCancelRequest);
        let request = envelope
            .payload_as_leave_cancel_request()
            .expect("the payload is a cancellation request");
        assert_eq!(
            (request._tab.vtable().num_bytes() - 4) / 2,
            0,
            "LeaveCancelRequest must not carry a deadline or outcome"
        );

        assert_eq!(
            decode(&encode_leave_cancel_result(true, 0)),
            Ok(Message::LeaveCancelResult(LeaveCancelResult {
                accepted: true,
                remaining_ms: 0,
            }))
        );
        assert_eq!(
            decode(&encode_leave_cancel_result(false, 7_250)),
            Ok(Message::LeaveCancelResult(LeaveCancelResult {
                accepted: false,
                remaining_ms: 7_250,
            }))
        );
        for (accepted, remaining_ms) in [(true, 1), (false, 0)] {
            assert_eq!(
                decode(&encode_leave_cancel_result(accepted, remaining_ms)),
                Err(DecodeError::LeaveCancelResultShape {
                    accepted,
                    remaining_ms,
                })
            );
        }
    }

    // -----------------------------------------------------------------------
    // Protocol V20 — chat and party wire foundation
    // -----------------------------------------------------------------------

    #[test]
    fn chat_and_party_requests_copy_display_text_verbatim() {
        let chat = ChatRequest {
            text: "  skål\n".to_owned(),
        };
        let frame = encode_chat_request(&chat);
        assert_eq!(decode(&frame), Ok(Message::ClientOnly("ChatRequest")));
        let envelope = fb::root_as_envelope(&frame).expect("valid chat request");
        assert_eq!(
            envelope
                .payload_as_chat_request()
                .and_then(|request| request.text()),
            Some(chat.text.as_str())
        );

        for action in [
            PartyAction::Invite,
            PartyAction::Accept,
            PartyAction::Decline,
            PartyAction::Leave,
            PartyAction::Kick,
        ] {
            let request = PartyRequest {
                action,
                target_name: "  Freya  ".to_owned(),
            };
            let frame = encode_party_request(&request);
            assert_eq!(decode(&frame), Ok(Message::ClientOnly("PartyRequest")));
            let envelope = fb::root_as_envelope(&frame).expect("valid party request");
            let wire = envelope
                .payload_as_party_request()
                .expect("PartyRequest payload");
            assert_eq!(wire.action(), action.wire());
            assert_eq!(wire.target_name(), Some(request.target_name.as_str()));
        }
    }

    #[test]
    fn chat_message_requires_identity_and_a_present_name() {
        assert_eq!(
            decode(&encode_chat_message(41, Some(""), None)),
            Ok(Message::Chat(ChatMessage {
                sender_entity_id: 41,
                sender_name: String::new(),
                text: String::new(),
            }))
        );
        assert_eq!(
            decode(&encode_chat_message(0, Some("Eir"), Some("hail"))),
            Err(DecodeError::ChatWithoutEntity)
        );
        assert_eq!(
            decode(&encode_chat_message(41, None, Some("hail"))),
            Err(DecodeError::ChatWithoutName(41))
        );
    }

    #[test]
    fn party_invite_requires_identity_name_and_time() {
        assert_eq!(
            decode(&encode_party_invite(72, Some(""), 15_000)),
            Ok(Message::PartyInvite(PartyInvite {
                from_entity_id: 72,
                from_name: String::new(),
                expires_ms: 15_000,
            }))
        );
        assert_eq!(
            decode(&encode_party_invite(0, Some("Sif"), 1)),
            Err(DecodeError::PartyInviteWithoutEntity)
        );
        assert_eq!(
            decode(&encode_party_invite(72, None, 1)),
            Err(DecodeError::PartyInviteWithoutName(72))
        );
        assert_eq!(
            decode(&encode_party_invite(72, Some("Sif"), 0)),
            Err(DecodeError::PartyInviteWithoutTime)
        );
    }

    #[test]
    fn snapshot_party_projection_enforces_frame_only_invariants() {
        let member = PartyMemberStateWire {
            entity_id: 17,
            pos: [1.5, 62.0, -8.0],
            health: 44,
            max_health: 50,
            alive: true,
        };
        assert_eq!(
            decode(&encode_entity_snapshot_with_party(91, &[member])),
            Ok(Message::Snapshot(Snapshot {
                server_tick: 1,
                party_leader_entity_id: 91,
                party_members: vec![PartyMemberState {
                    entity_id: 17,
                    pos: member.pos,
                    health: 44,
                    max_health: 50,
                    alive: true,
                }],
                party_roster: vec![
                    PartyRosterMember {
                        character_id: 1,
                        entity_id: 91,
                        name: "Leader".to_owned(),
                        online: true,
                    },
                    PartyRosterMember {
                        character_id: 2,
                        entity_id: 17,
                        name: "Member".to_owned(),
                        online: true,
                    },
                ],
                ..Default::default()
            }))
        );
        // The frame decoder has no recipient id: a leader absent from members may be
        // the recipient, including a party with no other members.
        assert!(decode(&encode_entity_snapshot_with_party(91, &[])).is_ok());
        assert_eq!(
            decode(&encode_entity_snapshot_with_party(0, &[member])),
            Err(DecodeError::PartyMembersWithoutLeader)
        );

        for (broken, want) in [
            (
                PartyMemberStateWire {
                    entity_id: 0,
                    ..member
                },
                DecodeError::PartyMemberWithoutIdentity,
            ),
            (
                PartyMemberStateWire {
                    health: 51,
                    ..member
                },
                DecodeError::PartyMemberHealth {
                    entity_id: 17,
                    health: 51,
                    max_health: 50,
                },
            ),
        ] {
            assert_eq!(
                decode(&encode_entity_snapshot_with_party(91, &[broken])),
                Err(want)
            );
        }
        assert!(matches!(
            decode(&encode_entity_snapshot_with_party(
                91,
                &[PartyMemberStateWire {
                    pos: [f32::NAN, 0.0, 0.0],
                    ..member
                }]
            )),
            Err(DecodeError::NonFinitePartyMember {
                entity_id: 17,
                field: "pos.x",
                value,
            }) if value.is_nan()
        ));
        assert_eq!(
            decode(&encode_entity_snapshot_with_party(17, &[member, member])),
            Err(DecodeError::DuplicatePartyMember(17))
        );
    }

    // -----------------------------------------------------------------------
    // Protocol V21 — persistent roster and corpse loot contract
    // -----------------------------------------------------------------------

    #[test]
    fn loot_requests_carry_only_identity_revision_and_ordering() {
        let open = LootOpenRequest {
            corpse_id: 400,
            client_tick: 91,
        };
        let frame = encode_loot_open_request(&open);
        assert_eq!(decode(&frame), Ok(Message::ClientOnly("LootOpenRequest")));
        let envelope = fb::root_as_envelope(&frame).expect("valid loot-open request");
        let wire = envelope
            .payload_as_loot_open_request()
            .expect("LootOpenRequest payload");
        assert_eq!((wire.corpse_id(), wire.client_tick()), (400, 91));

        let take = LootTakeRequest {
            corpse_id: 400,
            entry_id: 7,
            revision: 3,
            client_tick: 92,
        };
        let frame = encode_loot_take_request(&take);
        assert_eq!(decode(&frame), Ok(Message::ClientOnly("LootTakeRequest")));
        let envelope = fb::root_as_envelope(&frame).expect("valid loot-take request");
        let wire = envelope
            .payload_as_loot_take_request()
            .expect("LootTakeRequest payload");
        assert_eq!(
            (
                wire.corpse_id(),
                wire.entry_id(),
                wire.revision(),
                wire.client_tick()
            ),
            (400, 7, 3, 92)
        );
    }

    /// **V23's take-all carries identity, revision and ordering — and no entry.**
    ///
    /// The absence is the contract: an entry id would be this side naming what comes home,
    /// and a count would be it naming how much. The server walks its own container.
    #[test]
    fn the_take_all_request_names_no_entry_and_no_count() {
        let take_all = LootTakeAllRequest {
            corpse_id: 400,
            revision: 3,
            client_tick: 93,
        };
        let frame = encode_loot_take_all_request(&take_all);
        assert_eq!(
            decode(&frame),
            Ok(Message::ClientOnly("LootTakeAllRequest"))
        );
        let envelope = fb::root_as_envelope(&frame).expect("valid loot-take-all request");
        assert_eq!(envelope.payload_type(), fb::Payload::LootTakeAllRequest);
        let wire = envelope
            .payload_as_loot_take_all_request()
            .expect("LootTakeAllRequest payload");
        assert_eq!(
            (wire.corpse_id(), wire.revision(), wire.client_tick()),
            (400, 3, 93)
        );
    }

    #[test]
    fn loot_state_is_complete_and_rejects_invalid_entries() {
        let entries = [
            LootEntry {
                entry_id: 7,
                item_id: 31,
                count: 4,
                durability: 0,
                max_durability: 0,
            },
            LootEntry {
                entry_id: 8,
                item_id: 9,
                count: 1,
                durability: 3,
                max_durability: 10,
            },
        ];
        assert_eq!(
            decode(&encode_loot_state(400, 2, Some(&entries))),
            Ok(Message::LootState(LootState {
                corpse_id: 400,
                revision: 2,
                entries: entries.to_vec(),
                silver: 0,
            }))
        );
        assert_eq!(
            decode(&encode_loot_state(0, 2, Some(&entries))),
            Err(DecodeError::LootWithoutCorpse("LootState"))
        );
        assert_eq!(
            decode(&encode_loot_state(400, 0, Some(&entries))),
            Err(DecodeError::LootWithoutRevision)
        );
        for absent in [None, Some(&[][..])] {
            assert_eq!(
                decode(&encode_loot_state(400, 2, absent)),
                Err(DecodeError::LootWithoutEntries(400))
            );
        }

        let invalid = LootEntry {
            entry_id: 0,
            ..entries[0]
        };
        assert_eq!(
            decode(&encode_loot_state(400, 2, Some(&[invalid]))),
            Err(DecodeError::LootEntryWithoutIdentity(400))
        );
        assert_eq!(
            decode(&encode_loot_state(400, 2, Some(&[entries[0], entries[0]]))),
            Err(DecodeError::DuplicateLootEntry {
                corpse_id: 400,
                entry_id: 7,
            })
        );
        let invalid = LootEntry {
            entry_id: 9,
            item_id: 12,
            count: 2,
            durability: 1,
            max_durability: 10,
        };
        assert!(matches!(
            decode(&encode_loot_state(400, 2, Some(&[invalid]))),
            Err(DecodeError::InvalidLootEntry { entry_id: 9, .. })
        ));
    }

    #[test]
    fn mob_hit_requires_identity_and_a_finite_present_position() {
        let hit = MobHit {
            attacker_entity_id: 41,
            attacker_pos: [1.5, 64.0, -2.5],
        };
        assert_eq!(
            decode(&encode_mob_hit(
                hit.attacker_entity_id,
                Some(hit.attacker_pos)
            )),
            Ok(Message::MobHit(hit))
        );
        assert_eq!(
            decode(&encode_mob_hit(0, Some([0.0; 3]))),
            Err(DecodeError::MobHitWithoutIdentity)
        );
        assert_eq!(
            decode(&encode_mob_hit(41, None)),
            Err(DecodeError::MissingMobHitPosition)
        );
        assert!(matches!(
            decode(&encode_mob_hit(41, Some([0.0, f32::NAN, 0.0]))),
            Err(DecodeError::NonFiniteMobHit {
                attacker_entity_id: 41,
                axis: 1,
                value,
            }) if value.is_nan()
        ));
    }

    #[test]
    fn loot_closed_requires_the_corpse_identity() {
        assert_eq!(
            decode(&encode_loot_closed(400)),
            Ok(Message::LootClosed(LootClosed { corpse_id: 400 }))
        );
        assert_eq!(
            decode(&encode_loot_closed(0)),
            Err(DecodeError::LootWithoutCorpse("LootClosed"))
        );
    }

    #[test]
    fn an_offline_leader_and_online_members_share_one_stable_roster() {
        let member = PartyMemberStateWire {
            entity_id: 17,
            pos: [1.0, 2.0, 3.0],
            health: 40,
            max_health: 50,
            alive: true,
        };
        let roster = [
            PartyRosterMemberWire {
                character_id: 101,
                entity_id: 0,
                name: Some("Leader"),
                online: false,
            },
            PartyRosterMemberWire {
                character_id: 202,
                entity_id: 17,
                name: Some("Member"),
                online: true,
            },
        ];
        let decoded = decode(&encode_entity_snapshot_with_roster(
            0,
            &[member],
            &roster,
            &[800],
        ));
        let Ok(Message::Snapshot(snapshot)) = decoded else {
            panic!("offline leader snapshot did not decode: {decoded:?}");
        };
        assert_eq!(snapshot.party_leader_entity_id, 0);
        assert_eq!(snapshot.party_roster[0].character_id, 101);
        assert!(!snapshot.party_roster[0].online);
        assert_eq!(snapshot.party_members[0].entity_id, 17);
        assert_eq!(snapshot.accessible_loot_corpses, vec![800]);
        assert!(
            snapshot
                .mobs
                .iter()
                .any(|mob| mob.entity_id == 800 && mob.action == MobAction::Corpse)
        );

        assert_eq!(
            decode(&encode_entity_snapshot_with_roster(
                99,
                &[member],
                &roster,
                &[]
            )),
            Err(DecodeError::PartyLeaderRosterMismatch {
                expected: 0,
                actual: 99,
            })
        );
        assert_eq!(
            decode(&encode_entity_snapshot_with_roster(
                0,
                &[PartyMemberStateWire {
                    entity_id: 18,
                    ..member
                }],
                &roster,
                &[]
            )),
            Err(DecodeError::PartyMemberMissingFromRoster(18))
        );
        let broken = [PartyRosterMemberWire {
            entity_id: 0,
            online: true,
            ..roster[0]
        }];
        assert_eq!(
            decode(&encode_entity_snapshot_with_roster(0, &[], &broken, &[])),
            Err(DecodeError::PartyRosterOnlineMismatch {
                character_id: 101,
                entity_id: 0,
                online: true,
            })
        );
    }

    #[test]
    fn a_snapshot_decodes_its_mobs_in_wire_order() {
        let mobs = [
            MobStateWire {
                vel: Some([1.0, -2.0, 0.5]),
                yaw: 1.25,
                health: 35,
                action: fb::MobAction::Chase,
                // No entity with this id is present in the snapshot. That is valid:
                // the hunted player can be outside this recipient's visibility.
                target_entity_id: 777,
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
                    target_entity_id: 777,
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
                    target_entity_id: 0,
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

    #[test]
    fn a_snapshot_carrying_a_fleeing_deer_decodes() {
        let deer = MobStateWire {
            kind: fb::MobKind::Deer,
            health: 20,
            max_health: 20,
            action: fb::MobAction::Flee,
            ..MobStateWire::draugr(903, 4.0)
        };

        let Ok(Message::Snapshot(snapshot)) = snapshot_of(&[deer], PlayerVitalsWire::default())
        else {
            panic!("a snapshot carrying a fleeing deer did not decode");
        };
        assert_eq!(snapshot.mobs[0].kind, MobKind::Deer);
        assert_eq!(snapshot.mobs[0].action, MobAction::Flee);
    }

    #[test]
    fn a_snapshot_carrying_a_paddock_horse_decodes() {
        let horse = MobStateWire {
            kind: fb::MobKind::Horse,
            health: 100,
            max_health: 100,
            action: fb::MobAction::Idle,
            ..MobStateWire::draugr(904, 4.0)
        };

        let Ok(Message::Snapshot(snapshot)) = snapshot_of(&[horse], PlayerVitalsWire::default())
        else {
            panic!("a snapshot carrying a horse did not decode");
        };
        assert_eq!(snapshot.mobs[0].kind, MobKind::Horse);
        assert_eq!(snapshot.mobs[0].action, MobAction::Idle);
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
                lit: true,
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
                hunger: 40,
                max_hunger: 100,
                level: 7,
                experience: 20,
                experience_to_next: 350,
                life_state: fb::LifeState::Dead,
                respawn_ticks: 60,
                invulnerable: false,
                blocking: false,
            },
        ) else {
            panic!("a valid dead player's snapshot did not decode");
        };

        assert_eq!(
            snapshot.self_vitals,
            PlayerVitals {
                health: 0,
                max_health: 100,
                hunger: 40,
                max_hunger: 100,
                level: 7,
                experience: 20,
                experience_to_next: 350,
                life_state: LifeState::Dead,
                respawn_ticks: 60,
                invulnerable: false,
                blocking: false,
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

    /// Equality is the level-cap representation the contract promises. The client
    /// knows no curve and needs no cap special case: it only checks the wire invariant.
    #[test]
    fn capped_experience_may_equal_its_denominator() {
        let Ok(Message::Snapshot(snapshot)) = snapshot_of(
            &[],
            PlayerVitalsWire {
                level: 30,
                experience: 1_500,
                experience_to_next: 1_500,
                ..PlayerVitalsWire::default()
            },
        ) else {
            panic!("the contract's capped progression shape did not decode");
        };

        assert_eq!(snapshot.self_vitals.level, 30);
        assert_eq!(snapshot.self_vitals.experience, 1_500);
        assert_eq!(snapshot.self_vitals.experience_to_next, 1_500);
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
                "a zero hunger maximum, which no presentation may divide by",
                PlayerVitalsWire {
                    hunger: 0,
                    max_hunger: 0,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::VitalsHunger {
                    hunger: 0,
                    max_hunger: 0,
                },
            ),
            (
                "more hunger than the maximum",
                PlayerVitalsWire {
                    hunger: 101,
                    max_hunger: 100,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::VitalsHunger {
                    hunger: 101,
                    max_hunger: 100,
                },
            ),
            (
                "level zero, which no character can have",
                PlayerVitalsWire {
                    level: 0,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::VitalsExperience {
                    level: 0,
                    experience: 0,
                    experience_to_next: 50,
                },
            ),
            (
                "a zero experience denominator, which no presentation may divide by",
                PlayerVitalsWire {
                    experience_to_next: 0,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::VitalsExperience {
                    level: 1,
                    experience: 0,
                    experience_to_next: 0,
                },
            ),
            (
                "more experience than the level requires",
                PlayerVitalsWire {
                    experience: 51,
                    experience_to_next: 50,
                    ..PlayerVitalsWire::default()
                },
                DecodeError::VitalsExperience {
                    level: 1,
                    experience: 51,
                    experience_to_next: 50,
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
                    lit: true,
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
                    lit: true,
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
                silver: 0,
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
                silver: 0,
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
