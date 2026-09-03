//! The network boundary.
//!
//! One socket, on one dedicated `std::thread`, talking to the ECS through
//! `std::sync::mpsc`. Nothing outside this module touches a socket and nothing
//! inside it touches rendering — the same one-way split the server keeps between
//! `internal/transport` and `internal/game`.
//!
//! ## The thread boundary
//!
//! A Bevy system must never block, and a socket read blocks by definition. So the
//! socket lives on [`session`]'s thread, which owns it exclusively, and the two
//! sides exchange values rather than access:
//!
//! ```text
//!   ECS (this module)                     net thread (session.rs)
//!   ─────────────────                     ───────────────────────
//!   Receiver<SessionEvent>  ◀── mpsc ───  Sender<SessionEvent>
//!   Sender<NetCommand>      ─── mpsc ──▶  Receiver<NetCommand>
//! ```
//!
//! `drain_session_events` empties the receiver with `try_recv` every frame and
//! returns; it cannot wait, and there is no code path on which it does. The
//! command channel carries the one instruction this issue has ("stop"), which the
//! ECS sends by dropping its end — see [`Channels::drop`].
//!
//! There is no outbound *frame* channel yet, deliberately: `PlayerInput` is the
//! first message the ECS will originate and it belongs to the movement issue.
//! When it lands, the shape to copy is the server's — one reader and one writer
//! per connection — which means a second thread draining a `Receiver<Vec<u8>>`,
//! not a system that writes to a socket.
//!
//! ## What the client is allowed to conclude
//!
//! Nothing. The server is authoritative: this module transports bytes and
//! publishes what the server said. [`SessionParams`] carries the server's
//! answers; it never carries a decision made here.

mod codec;
mod frame;
mod handshake;
mod http;
mod json;
mod servers;
mod session;
mod signin;
mod tickets;
mod tls;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use bevy::prelude::*;

pub use codec::BlockRequest;
#[allow(unused_imports)] // V20 protocol surface; ECS consumers land in later issues.
pub use codec::{
    ActionRefused, Appearance, AttackRequest, BlockCoord, BlockEditRequest, CHUNK_COLUMN_BLOCKS,
    CharacterSummary, ChatMessage, ChatRequest, ChunkCoord, ConsumeRequest, CraftRequest,
    DropItemRequest, EditAction, EntityState, Facing, HairModel, InventoryMoveRequest,
    InventoryStack, InventoryState, ItemDropState, LifeState, LootClosed, LootEntry,
    LootOpenRequest, LootState, LootTakeAllRequest, LootTakeRequest, MAP_TILE_CELLS, MAP_TILE_EDGE,
    MARKER_NOTE_MAX_BYTES, MAX_MARKERS, MAX_VIEW_DISTANCE, MapColumn, MapExplored, MapSurface,
    MapTile, MapTileRequest, Marker, MarkerKind, MarkerList, MarkerPlaceRequest,
    MarkerRemoveRequest, MineProgress, MineRequest, MobAction, MobHit, MobKind, MobState,
    PLACEHOLDER_APPEARANCE, PartyAction, PartyInvite, PartyMemberState, PartyRequest,
    PartyRosterMember, PlaceStructureRequest, PlayerAppearance, PlayerInput, PlayerVitals,
    ProjectileKind, ProjectileState, RecipeId, RefusalReason, RefusedAction, Reject,
    RemoveStructureRequest, RepairRequest, SessionParams, Snapshot, StructureKind, StructureState,
    WorldClock, WorldUpdate, map_tile_explored_bytes, map_tile_span,
};
// V27's stable contract, ahead of the server and presentation consumers that fill it.
#[cfg(test)]
pub use codec::MountState;
pub use codec::{CastKind, CastState, LearnedMounts, MountKind, MountRequest};
// V28's player-trade protocol surface, ahead of the presentation consumer. The
// authoritative state is decoded here; no client system decides a trade outcome.
#[allow(unused_imports)]
pub use codec::{
    PLAYER_TRADE_SLOTS, PlayerTradeAction, PlayerTradeCloseReason, PlayerTradeClosed,
    PlayerTradeRequest, PlayerTradeSlot, PlayerTradeState,
};
// V25's settlement surface, ahead of the consumers that read it: the resident window is
// #458 and the vendor window is #459. Named here for the reason the block above is —
// so neither issue has to reopen `codec.rs` to find out what it is allowed to spell.
#[allow(unused_imports)] // V25 protocol surface; ECS consumers land in #458 and #459.
pub use codec::{
    NpcInteractRequest, RESIDENT_NAME_MAX_BYTES, ResidentAppearance, ResidentRole, TradeRequest,
    VendorClosed, VendorEntry, VendorState,
};
// V26's Fimbulvetr surface. Named here for the reason the two blocks above are: a
// presentation consumer should not have to reopen `codec.rs` to find out what it is
// allowed to spell.
#[allow(unused_imports)] // The contract bound remains public for later ward consumers.
pub use codec::{
    MAX_WARDED_COLUMNS, StormPhase, StormWarning, WardKind, WardedColumn, WardsNearby, WeatherKind,
    WeatherState,
};

// `PlayerToken` itself is deliberately not re-exported: outside this module the
// token is a field nobody reads, and a name nothing outside `net` can spell is a
// name nothing outside `net` can start deciding from.
#[cfg(test)]
pub use codec::ANY_TOKEN;
pub use codec::encode_block_request;
#[allow(unused_imports)] // V28 outbound encoder precedes the player-trade UI.
pub use codec::encode_player_trade_request;
#[allow(unused_imports)] // V20 outbound encoders precede their UI controls.
pub use codec::{
    encode_attack_request, encode_block_edit_request, encode_chat_request,
    encode_chunk_resend_request, encode_consume_request, encode_craft_request,
    encode_drop_item_request, encode_inventory_move_request, encode_loot_open_request,
    encode_loot_take_all_request, encode_loot_take_request, encode_map_tile_request,
    encode_marker_place_request, encode_marker_remove_request, encode_mine_request,
    encode_party_request, encode_place_structure_request, encode_player_input,
    encode_remove_structure_request, encode_repair_request,
};
pub use codec::{encode_dismount_request, encode_mount_request};
#[allow(unused_imports)] // V25 outbound encoders precede their UI controls (#458, #459).
pub use codec::{encode_npc_interact_request, encode_trade_request};
pub use servers::ListedServer;
use servers::ServerListEvent;
use session::{Choice, NetCommand, SessionEvent};

pub use signin::AccountService;
use signin::{SignInCommand, SignInEvent};

/// How many frames may wait for the writer thread before the ECS starts dropping them.
///
/// Deliberately shallow, and the mirror of `session.outboundQueue` on the server. What
/// waits here is input, and an input frame describes the controls *now*: by the time a
/// deep queue drained, every frame in it would be describing a tick that had passed. So
/// the queue is short and a full one loses the frame rather than the ECS blocking on a
/// socket — which a Bevy system must never do.
const OUTBOUND_QUEUE: usize = 32;

/// The display name announced in `ClientHello` when nothing says otherwise.
///
/// Untrusted, non-unique and never an identifier — the server assigns
/// `ServerWelcome.entity_id` for that, and the identity behind a session is the
/// token, not this. `--name` and `VOXELHEIM_NAME` replace it; `main.rs` resolves
/// which of the three applies, exactly as it does for the address.
pub const DEFAULT_PLAYER_NAME: &str = "voxelheim";

/// Where the connection has got to. The status text is a rendering of this and
/// nothing else.
///
/// Two of the eight variants are terminal-with-a-reason and terminal-without-one,
/// and the difference is worth stating: [`Self::Rejected`] means *there is no
/// session and here is why* — a `ServerReject`, an unreachable address, or a peer
/// that turned out not to speak this protocol. [`Self::Disconnected`] means a
/// session that existed has ended; by then the player has seen a world and the
/// detail belongs in the log.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// No server has been chosen, so nothing has been dialled. The state a client
    /// with an account service starts in and the one the server list is up over.
    ///
    /// It exists because the address stopped being a launch setting: a client that
    /// reads its servers from a list has no socket to open until somebody clicks one,
    /// and calling that `Connecting` would have the status line claim an attempt that
    /// nothing had made. There is no [`ServerAddress`] while this is the state, for the
    /// same reason.
    Idle,
    /// Opening the socket.
    Connecting,
    /// The socket is up and `ClientHello` is on the wire.
    Handshaking,
    /// The account's characters have arrived and one of them is being chosen.
    ///
    /// **The one state this client spends waiting for a person**, which is why it is a
    /// state of its own rather than a longer `Handshaking`: what the status line should
    /// say is that the game is waiting for the player, not that it is waiting for the
    /// server. [`CharacterChoice`] carries the list while this is the state.
    Choosing,
    /// A validated `ServerWelcome` arrived. [`Session`] exists.
    Connected,
    /// A leave was requested. `None` is the short interval before the server
    /// acknowledges it; `Some` is the server-owned remaining whole seconds.
    Leaving { seconds_remaining: Option<u32> },

    /// There is no session. `reason` is written for a player to read.
    Rejected { reason: String },
    /// A session that existed has ended.
    Disconnected,
}

#[cfg(test)]
impl ConnectionState {
    /// Every state, for the sweeps that have to answer for all of them.
    ///
    /// One list rather than one per sweep. Two screens decide what they do from this
    /// enum — the server list is up or down, the status line names the address or does
    /// not — and each carried its own list of states to try. `Choosing` was added to the
    /// enum in #160 and reached neither, so for two iterations both sweeps read as
    /// exhaustive while leaving the one state this client spends waiting for a person
    /// untested.
    ///
    /// **What pins it is `the_list_holds_every_state` below**, which sets a flag per
    /// variant through a wildcard-free match. A ninth variant stops that match
    /// compiling, the arm written for it indexes past the flags, and the only edit that
    /// makes the assertion pass again is adding the state here — so the list cannot fall
    /// behind the enum, and no number can be bumped to quieten it.
    ///
    /// That is a stronger pin than `HairModel::ALL` gets from the compiler alone, and it
    /// is available for the reason the other one needs a contract: the variants are
    /// nameable here, because `ConnectionState` is this client's own vocabulary.
    pub(crate) fn every() -> Vec<Self> {
        vec![
            Self::Idle,
            Self::Connecting,
            Self::Handshaking,
            Self::Choosing,
            Self::Connected,
            Self::Leaving {
                seconds_remaining: None,
            },
            Self::Rejected {
                reason: "refused".to_owned(),
            },
            Self::Disconnected,
        ]
    }
}

/// What the client may say about cancelling a leave before the server accepts it.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveCancellation {
    /// Escape may originate one request.
    Available,
    /// A request is in flight; controls remain inert.
    Pending,
    /// The server refused the last request; the countdown remains authoritative.
    Refused,
}

/// The authoritative session parameters, present exactly when
/// [`ConnectionState::Connected`] has been reached.
///
/// Absence is the honest encoding of "there is no session", which is why this is
/// inserted on the handshake rather than being a resource holding an `Option`
/// that every reader would have to second-guess.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct Session(pub SessionParams);

/// Whether this session resumed the identity the client presented, or the server
/// answered with a different one.
///
/// **Display only.** Nothing branches on it but the status line, and nothing may:
/// the server had settled the identity before it sent the welcome, and a client
/// that decided anything from a token would be deciding something the server
/// already decided. It says which of two things happened, not what to do about it.
///
/// A resource of its own rather than a field on [`Session`], because the two have
/// different provenance. `Session` is the validated welcome and nothing else —
/// `SessionParams` can only be built by the codec, from bytes the server sent.
/// This is a comparison the net thread made between that welcome and a file, and
/// folding it into the same value would blur the line that makes the first one
/// trustworthy. It is inserted and removed exactly where `Session` is.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// The welcome carried the token that was presented: the same character.
    Returning,
    /// It carried a different one — a first connection to this server, a token it
    /// did not recognise, or a server that re-issued. All three are a new
    /// character as far as anything on screen can tell.
    New,
    /// **This session kept no identity file, so the comparison the other two report
    /// was never made** — and the honest answer is that the client does not know.
    ///
    /// It is what an `Unlisted` session reports, which is every session on the
    /// development path. There the account the ticket names is what decides which
    /// character comes back, and `schemas/handshake.fbs` is explicit that the server
    /// does not tell the client which of the two it did. So the third variant is not a
    /// missing value to be filled in later: it is the state of a client that presented
    /// no token and is entitled to no opinion.
    Untold,
}

/// The characters this account owns on this world, present exactly while one is being
/// chosen.
///
/// Inserted when the server answers the hello with them and removed the moment the
/// exchange ends — a welcome, a refusal or a session that went away — so its presence is
/// what the character screen is up on, the same shape [`Session`] has for a live session.
///
/// **Nothing in it is decided here.** The rows are the server's, the limit is the
/// server's, and `preselect` is this client's own note about which character it played
/// here last: a convenience read from a file, matched against the list, and worth exactly
/// one keypress. See `session::ChosenCharacter`.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct CharacterChoice {
    characters: Vec<CharacterSummary>,
    max_characters: u8,
    preselect: Option<u64>,
    answered: bool,
    attempted: Option<CharacterAttempt>,
    creation_refusal: Option<String>,
}

/// Which half of the character question the server is answering.
///
/// Private and deliberately smaller than [`ChooseCharacter`]: the ECS needs only to
/// know whether a character-name refusal answered a *creation*. A server sending the
/// same code after a selection or before any choice is not a retryable exchange, and
/// treating it as one would turn a broken peer into a redial loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharacterAttempt {
    Play,
    Create,
}

impl CharacterChoice {
    /// Every character this account holds here, in the order the server listed them.
    ///
    /// That order is the server's and carries no meaning a client may read — it is not
    /// recency, not creation order and not rank — so this screen shows it as given rather
    /// than sorting it into an opinion of its own.
    pub fn characters(&self) -> &[CharacterSummary] {
        &self.characters
    }

    /// How many characters this account may hold here, including the ones above.
    pub fn max_characters(&self) -> u8 {
        self.max_characters
    }

    /// Which character to start on: the one this client played here last, when it is
    /// still in the list. `None` starts on the first row.
    pub fn preselect(&self) -> Option<u64> {
        self.preselect
    }

    /// Whether this account may create another character here.
    ///
    /// The server refuses one past the limit with `CHARACTER_LIMIT_REACHED` whatever this
    /// answers; what it buys is a screen that does not offer what it has just been told
    /// is unavailable. It is the same courtesy a grayed-out recipe row is.
    pub fn has_room(&self) -> bool {
        self.characters.len() < usize::from(self.max_characters)
    }

    /// Whether a choice has already gone out for this exchange.
    ///
    /// **The one piece of state that keeps a second click from ending the session.** A
    /// welcome is the answer to a choice, so the server leaves the character phase the
    /// moment it takes one — and a second `SelectCharacterRequest` then arrives on a
    /// session that is in the world, where it is a protocol error and closes the
    /// connection. It lives here rather than on the screen so that *any* producer is
    /// covered by it, and the screen reads it to stop offering a control that would.
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// The server's answer to the last name this form submitted, while one exists.
    ///
    /// Display only. The code and detail are kept verbatim, and neither is parsed into a
    /// client-side name rule: the next name is sent to the server exactly as before.
    pub fn creation_refusal(&self) -> Option<&str> {
        self.creation_refusal.as_deref()
    }

    /// A pending choice without a socket, so the screen that draws one can be exercised
    /// headlessly. Test-only, for the reason `ListedServer::for_a_test` is: this is a
    /// value the server sends, and a system that could build one would be a client
    /// inventing its own characters.
    #[cfg(test)]
    pub fn for_a_test(characters: Vec<CharacterSummary>, max_characters: u8) -> Self {
        Self {
            characters,
            max_characters,
            preselect: None,
            answered: false,
            attempted: None,
            creation_refusal: None,
        }
    }

    /// The same, remembering a character this client played here before.
    #[cfg(test)]
    pub fn preselecting(mut self, character_id: u64) -> Self {
        self.preselect = Some(character_id);
        self
    }

    /// The same, already answered — what the boundary leaves behind once a choice has
    /// gone out, and the state a screen must offer nothing in.
    #[cfg(test)]
    pub fn already_answered(mut self) -> Self {
        self.answered = true;
        self
    }

    /// The retryable answer a re-opened exchange carries back to the form.
    #[cfg(test)]
    pub fn after_creation_refusal(mut self, reason: impl Into<String>) -> Self {
        self.creation_refusal = Some(reason.into());
        self
    }
}

/// The character screen answering the list: play this one, or make this one.
///
/// **A message rather than a frame, and the boundary is the point.** `ui` may ask; only
/// `net` may write to a socket — the same rule [`ConnectRequest`] and
/// [`DisconnectRequest`] follow. The frame itself is built and written on the session
/// thread, which is the only writer on that socket until the welcome.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub enum ChooseCharacter {
    /// Play the character this id names — one the server minted and listed.
    Play(u64),
    /// Make one and play it. What names a character is the *server's* rule, so an
    /// unacceptable name is a refusal with a reply rather than something judged here.
    Create {
        name: String,
        appearance: Appearance,
    },
}

/// Asks the network boundary to send one leave-cancellation intent.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelLeaveRequest;

/// The address this client dialled. Read by the UI so the status line can name it;
/// the net thread has its own copy.
///
/// **Present exactly while a session has been started**, which is why the status line
/// takes it as an `Option`: before a server is chosen there is no address, and a
/// resource holding an empty string would be this client inventing one. Inserted
/// beside [`ConnectionState::Connecting`] and left in place afterwards, so
/// "Disconnected from …" still has a name to use.
#[derive(Resource, Debug, Clone)]
pub struct ServerAddress(pub String);

/// Everything the server has said about the voxel world that the ECS has not
/// applied yet.
///
/// One ordered queue rather than one Bevy message type per kind, because ordering
/// *across* kinds is the property that matters: the server unloads a chunk before
/// it re-sends it, and a consumer that saw the two through separate buffers could
/// apply them in either order. A single `Vec` cannot get that wrong.
///
/// Filled by [`drain_session_events`] and emptied by the world module, which is
/// the only consumer. Both plugins `init_resource` it so neither depends on being
/// built first.
#[derive(Resource, Debug, Default)]
pub struct WorldInbox(Vec<WorldUpdate>);

impl WorldInbox {
    /// Takes everything queued, leaving the inbox empty.
    ///
    /// Returns an owned `Vec` rather than a `Drain`, so a caller can hold the
    /// updates while it borrows the resources it is applying them to.
    pub fn take(&mut self) -> Vec<WorldUpdate> {
        std::mem::take(&mut self.0)
    }

    /// How many updates are waiting. Test-only: production code takes them all.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.0.len()
    }

    /// Queues an update as the net thread would. Test-only: it exists so the world
    /// module can be driven without a socket, exactly as `app_with_manual_link`
    /// drives the drain without a server.
    #[cfg(test)]
    pub fn push(&mut self, update: WorldUpdate) {
        self.0.push(update);
    }
}

/// Every snapshot the server has sent that the ECS has not applied yet, each with the
/// moment it arrived.
///
/// A queue rather than a single slot, because the buffer that consumes it keeps the two
/// most recent: dropping all but the newest here would leave nothing to interpolate from
/// on any frame that happened to see two arrivals.
///
/// Filled by [`drain_session_events`] and emptied by the player module, which is the only
/// consumer — the same shape as [`WorldInbox`], for the same reason.
#[derive(Resource, Debug, Default)]
pub struct SnapshotInbox(Vec<(Snapshot, Instant)>);

impl SnapshotInbox {
    /// Takes everything queued, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<(Snapshot, Instant)> {
        std::mem::take(&mut self.0)
    }

    /// Queues a snapshot as the net thread would. Test-only: it exists so the player
    /// module can be driven without a socket.
    #[cfg(test)]
    pub fn push(&mut self, snapshot: Snapshot, at: Instant) {
        self.0.push((snapshot, at));
    }
}

/// Every complete inventory the server has sent and the player module has not applied yet.
///
/// A queue preserves wire order. The consumer may keep only the last state in one frame
/// because every message is complete and supersedes every one before it.
#[derive(Resource, Debug, Default)]
pub struct InventoryInbox(Vec<InventoryState>);

impl InventoryInbox {
    /// Takes every queued state, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<InventoryState> {
        std::mem::take(&mut self.0)
    }

    /// Queues a state as the net thread would. Test-only so the player module can be
    /// driven without a socket.
    #[cfg(test)]
    pub fn push(&mut self, state: InventoryState) {
        self.0.push(state);
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.0.len()
    }
}

/// Complete learned-mount sets awaiting the player presentation.
#[derive(Resource, Debug, Default)]
pub struct LearnedMountsInbox(Vec<LearnedMounts>);

impl LearnedMountsInbox {
    pub fn take(&mut self) -> Vec<LearnedMounts> {
        std::mem::take(&mut self.0)
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

/// Loot payloads not yet consumed by the player presentation, in wire order.
#[derive(Resource, Debug, Default)]
pub struct LootInbox(Vec<LootEvent>);

/// The two server-owned changes one open loot window can receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LootEvent {
    State(LootState),
    Closed(LootClosed),
}

impl LootInbox {
    pub fn take(&mut self) -> Vec<LootEvent> {
        std::mem::take(&mut self.0)
    }

    #[cfg(test)]
    pub fn push(&mut self, event: LootEvent) {
        self.0.push(event);
    }
}

/// Vendor payloads not yet consumed by the player presentation, in wire order.
#[derive(Resource, Debug, Default)]
pub struct VendorInbox(Vec<VendorEvent>);

/// The two server-owned changes one open stall can receive.
///
/// [`LootEvent`]'s shape on the other revisioned session this client keeps, and one queue
/// rather than two for its reason: a `VendorClosed` that arrives after a `VendorState`
/// ends the window, and one that arrives before it does not, so the pair cannot be sorted
/// into separate inboxes without losing which happened first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorEvent {
    State(VendorState),
    Closed(VendorClosed),
}

impl VendorInbox {
    pub fn take(&mut self) -> Vec<VendorEvent> {
        std::mem::take(&mut self.0)
    }

    #[cfg(test)]
    pub fn push(&mut self, event: VendorEvent) {
        self.0.push(event);
    }
}

/// Player-trade payloads awaiting the authoritative session mirror, in wire order.
#[derive(Resource, Debug, Default)]
pub struct PlayerTradeInbox(Vec<PlayerTradeEvent>);

/// The two server-owned changes one player trade can receive.
///
/// One queue preserves the only ordering that matters: a close after a state ends that
/// view, while the reverse order opens the newer one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerTradeEvent {
    State(PlayerTradeState),
    Closed(PlayerTradeClosed),
}

impl PlayerTradeInbox {
    pub fn take(&mut self) -> Vec<PlayerTradeEvent> {
        std::mem::take(&mut self.0)
    }

    #[cfg(test)]
    pub fn push(&mut self, event: PlayerTradeEvent) {
        self.0.push(event);
    }
}

/// Map payloads not yet consumed by the map screen, in wire order.
///
/// Order is what makes these one queue rather than three: a `MapExplored` that arrives
/// after a tile evicts it, and one that arrives before it does not, so the two cannot be
/// sorted into separate inboxes without losing which happened first. A `MarkerList` joins
/// them for the weaker reason that it is the same consumer and the same session rule --
/// each list replaces the last outright, so nothing about it depends on order.
///
/// It carries no session lifetime of its own, and no inbox in this module does: each is
/// drained unconditionally every `Update` by its consumer, with no run condition, so an
/// inbox is a one-frame queue and cannot hold anything across a boundary that takes many
/// frames to cross. The map data that *does* outlive a frame is the tile cache the screen
/// reads, and that is where the session rule belongs, because a tile is drawn for one
/// character in one world.
#[derive(Resource, Debug, Default)]
pub struct MapInbox(Vec<MapEvent>);

/// The server-owned things the map screen receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapEvent {
    /// One square, drawn for this character at the scale it was asked for.
    Tile(MapTile),
    /// One page of the ledger of where this character has been.
    Explored(MapExplored),
    /// Every mark this character holds, **replacing** the screen's copy wholesale.
    Markers(MarkerList),
}

/// Enough room for a full screen of tiles and the ledger pages a join sends, while an
/// overload still discards the oldest rather than growing without limit. A dropped tile
/// costs one re-request; a dropped page costs an eviction that the next page redoes.
const MAP_INBOX_CAPACITY: usize = 256;

impl MapInbox {
    fn push_bounded(&mut self, event: MapEvent) {
        if self.0.len() == MAP_INBOX_CAPACITY {
            self.0.remove(0);
        }
        self.0.push(event);
    }

    /// Takes every queued payload in wire order, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<MapEvent> {
        std::mem::take(&mut self.0)
    }

    #[cfg(test)]
    pub fn push(&mut self, event: MapEvent) {
        self.push_bounded(event);
    }
}

/// Hit events awaiting presentation, bounded so a stalled frame cannot grow without limit.
#[derive(Resource, Debug, Default)]
pub struct MobHitInbox(Vec<MobHit>);

/// Enough room for a burst while still making overload discard stale presentation data.
const MOB_HIT_INBOX_CAPACITY: usize = 64;

impl MobHitInbox {
    fn push_bounded(&mut self, hit: MobHit) {
        if self.0.len() == MOB_HIT_INBOX_CAPACITY {
            self.0.remove(0);
        }
        self.0.push(hit);
    }

    /// Takes every queued hit in wire order, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<MobHit> {
        std::mem::take(&mut self.0)
    }

    #[cfg(test)]
    pub fn push(&mut self, hit: MobHit) {
        self.push_bounded(hit);
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.0.len()
    }
}

/// Authoritative mining progress not yet consumed by the player presentation.
///
/// Ordered like the wire. The player may keep only the newest entry in one frame,
/// because every value is a complete server answer for the voxel it names.
#[derive(Resource, Debug, Default)]
pub struct MineProgressInbox(Vec<MineProgress>);

impl MineProgressInbox {
    /// Takes every queued progress report, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<MineProgress> {
        std::mem::take(&mut self.0)
    }

    /// Queues a report as the net thread would. Test-only so presentation can be
    /// exercised without the mining server issue having landed.
    #[cfg(test)]
    pub fn push(&mut self, progress: MineProgress) {
        self.0.push(progress);
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.0.len()
    }
}

/// Every appearance the net thread has decoded and the player module has not read yet.
///
/// A queue like the three above it, and drained the same way. **The order matters and is
/// the wire's**: two appearances for one entity are the server correcting itself, and the
/// one that means anything is the later one.
///
/// It is deliberately *not* keyed by entity here. A queue is what crossed the thread
/// boundary; the cache of who looks like what belongs to the module that draws bodies,
/// because it is that module that knows when a body has gone and the entry may go with it.
#[derive(Resource, Debug, Default)]
pub struct AppearanceInbox(Vec<PlayerAppearance>);

impl AppearanceInbox {
    /// Takes every queued appearance, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<PlayerAppearance> {
        std::mem::take(&mut self.0)
    }

    /// Queues one as the net thread would. Test-only, so bodies can be dressed without
    /// a socket.
    #[cfg(test)]
    pub fn push(&mut self, appearance: PlayerAppearance) {
        self.0.push(appearance);
    }
}

/// Every resident description the net thread has decoded and the player module has not
/// read yet.
///
/// [`AppearanceInbox`] exactly, for a second kind of body: a resident is described once
/// per session as it first enters the view, the message is not ordered against the
/// snapshot stream, and the cache of who looks like what belongs to the module that
/// draws bodies rather than here.
///
/// A queue of its own rather than a widened one, because the two messages carry different
/// things — a player has a level and equipment, a resident has a role and neither — and a
/// sum type here would only be taken apart again on the other side.
#[derive(Resource, Debug, Default)]
pub struct ResidentInbox(Vec<ResidentAppearance>);

impl ResidentInbox {
    /// Takes every queued description, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<ResidentAppearance> {
        std::mem::take(&mut self.0)
    }

    /// Queues one as the net thread would. Test-only, so a villager can be given a name
    /// without a socket.
    #[cfg(test)]
    pub fn push(&mut self, resident: ResidentAppearance) {
        self.0.push(resident);
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.0.len()
    }
}

/// Every refusal the server has sent and the UI has not shown yet.
///
/// A queue, like the three above it, and drained the same way — but the consumer keeps
/// the **newest** rather than merging: unlike an inventory, two refusals are two
/// different answers, and the one worth a line on screen is the one that just arrived.
///
/// The whole ECS surface of this message is one resource read by one system, which is
/// deliberate. A refusal is a sentence; nothing branches on it, nothing simulates from
/// it, and nothing about the world changes because one arrived.
#[derive(Resource, Debug, Default)]
pub struct RefusalInbox(Vec<ActionRefused>);

impl RefusalInbox {
    /// Takes every queued refusal, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<ActionRefused> {
        std::mem::take(&mut self.0)
    }

    /// Queues one as the net thread would. Test-only, so the status line can be driven
    /// without a socket.
    #[cfg(test)]
    pub fn push(&mut self, refused: ActionRefused) {
        self.0.push(refused);
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.0.len()
    }
}

/// Every storm warning the net thread has delivered and the presentation has not read.
///
/// Each value carries the instant it was decoded. The warning's seconds are the server's
/// statement and the instant is only the display anchor from which the HUD subtracts wall
/// time; neither is a client-side storm clock.
#[derive(Resource, Debug, Default)]
pub struct StormInbox(Vec<(StormWarning, Instant)>);

impl StormInbox {
    /// Takes every queued warning in wire order, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<(StormWarning, Instant)> {
        std::mem::take(&mut self.0)
    }

    /// Whether there is anything for the presentation to consume.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Queues one as the net thread would. Test-only, so the presentation can be driven
    /// at a fixed instant without a socket or a sleeping test.
    #[cfg(test)]
    pub fn push(&mut self, warning: StormWarning, at: Instant) {
        self.0.push((warning, at));
    }
}

/// Complete ward lists the net thread has delivered and the presentation has not read.
///
/// Each list replaces the one before it wholesale. The queue preserves wire order until
/// the player module consumes the batch, then that module keeps the last complete answer;
/// an empty list is therefore a real clearing answer rather than no answer.
#[derive(Resource, Debug, Default)]
pub struct WardsInbox(Vec<WardsNearby>);

impl WardsInbox {
    /// Whether a frame has delivered no ward answer.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Takes every complete list in wire order, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<WardsNearby> {
        std::mem::take(&mut self.0)
    }

    /// Drops answers that belonged to a session which is no longer current.
    fn clear(&mut self) {
        self.0.clear();
    }

    /// Queues one list as the net thread would. Test-only, so the renderer can be driven
    /// without a socket.
    #[cfg(test)]
    pub fn push(&mut self, wards: WardsNearby) {
        self.0.push(wards);
    }
}

/// One entry in the ordered conversation stream shown by the chat log.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEntry {
    Message(ChatMessage),
    PartyInvite(PartyInvite),
}

/// Every accepted world-chat line and party invitation not yet consumed by the log.
///
/// Both payloads share one inbox so their relative wire order survives an ECS frame.
/// Chat is a conversation, not a latest-state snapshot: collapsing or regrouping two
/// entries would silently rewrite it.
#[derive(Resource, Debug, Default)]
pub struct ChatInbox(Vec<ChatEntry>);

impl ChatInbox {
    /// Takes every queued entry in wire order, leaving the inbox empty.
    pub fn take(&mut self) -> Vec<ChatEntry> {
        std::mem::take(&mut self.0)
    }

    #[cfg(test)]
    pub fn push(&mut self, entry: ChatEntry) {
        self.0.push(entry);
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.0.len()
    }
}

/// How many times an established session has ended for a reason the player was not told,
/// since the UI last consumed the count.
///
/// **Carries no detail, deliberately.** The full error text belongs to tracing —
/// [`drain_session_events`] already logs it with `warn!`, per [`SessionEvent::Ended`] — and
/// what reaches the player is one fixed, sanitized sentence naming nothing about a socket,
/// a protocol byte or a file. A counter rather than a `bool` is what keeps a burst from
/// being silently collapsed into one line the way a flag would, even though one connection
/// can end at most once.
///
/// Only pushed for an ending that interrupted a session the player was actually inside
/// (`Connected` or `Leaving`) and that carried a reason (`Ended(Some(_))`): the ordinary,
/// reasonless close that follows a completed leave is not a failure, and a close while a
/// character was still being chosen has its own screen and its own sentence — see
/// `peer_closed` in `session.rs`.
#[derive(Resource, Debug, Default)]
pub struct SessionEndingInbox(u32);

impl SessionEndingInbox {
    /// Takes every queued notice, leaving the count at zero.
    pub fn take(&mut self) -> u32 {
        std::mem::take(&mut self.0)
    }

    fn push(&mut self) {
        self.0 = self.0.saturating_add(1);
    }

    /// Queues one notice as `drain_session_events` would. Test-only, so the UI consumer
    /// can be driven without a socket or a real established-and-broken session.
    #[cfg(test)]
    pub fn push_for_test(&mut self) {
        self.push();
    }
}

/// The ECS end of the frames this client sends.
///
/// Present exactly while there is a net thread to send to: [`drain_session_events`]
/// removes it once the session has ended, which closes the channel and is how the writer
/// thread learns to stop. A system that wants to send therefore takes it as an `Option`,
/// and its absence means "there is nowhere to send".
///
/// The `Mutex` is a type obligation rather than a synchronisation one, exactly as in
/// [`NetLink`]: a Bevy resource must be `Sync`. The one accessor takes `ResMut` and reaches
/// the contents with `get_mut`, so no lock is ever taken.
#[derive(Resource)]
pub struct Outbound(Mutex<SyncSender<Vec<u8>>>);

/// The writer set aside while a live leave makes gameplay inert.
#[derive(Resource)]
struct SuspendedOutbound(Outbound);

/// What became of a frame handed to [`Outbound::send`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// Queued for the writer thread.
    Queued,
    /// The queue was full and the frame was dropped. Not an error: see [`OUTBOUND_QUEUE`].
    Dropped,
    /// There is no net thread any more. The session has ended.
    Closed,
}

/// The pause menu asking the network thread to end this session.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisconnectRequest;

impl Outbound {
    fn sibling(&mut self) -> Self {
        let sender = match self.0.get_mut() {
            Ok(sender) => sender,
            Err(poisoned) => poisoned.into_inner(),
        };
        Self(Mutex::new(sender.clone()))
    }

    /// Hands one encoded frame to the writer thread, without ever blocking.
    ///
    /// `try_send` and not `send`: this is called from a Bevy system, and a system that can
    /// block on a socket is a frame that can stall on a network. A full queue costs the
    /// frame, which for input is the right trade — the next tick's frame supersedes it.
    pub fn send(&mut self, frame: Vec<u8>) -> Sent {
        let sender = match self.0.get_mut() {
            Ok(sender) => sender,
            // Recovered rather than propagated, for the reason NetLink gives: nothing here
            // panics while holding it, and a client that stopped sending input because of
            // an unrelated panic elsewhere would be a worse outcome than a recovered mutex.
            Err(poisoned) => poisoned.into_inner(),
        };

        match sender.try_send(frame) {
            Ok(()) => Sent::Queued,
            Err(TrySendError::Full(_)) => Sent::Dropped,
            Err(TrySendError::Disconnected(_)) => Sent::Closed,
        }
    }

    /// The ECS end of a channel whose far side is a test rather than a writer thread.
    ///
    /// Test-only, and it exists for the reason [`WorldInbox::push`] does: a module that
    /// *originates* frames has to be drivable without a socket. This module's own tests
    /// use a real one — they are about the boundary — but the player module's are about
    /// what it says, and reading the bytes a click produced is a stronger assertion than
    /// observing that it produced something.
    #[cfg(test)]
    pub fn to_a_test(capacity: usize) -> (Self, Receiver<Vec<u8>>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        (Self(Mutex::new(sender)), receiver)
    }
}

/// Orders systems that read what the net thread said after the system that reads
/// it off the channel.
///
/// Exported because the world module has to run after this and a private system
/// function cannot be named from outside. Ordering against an empty set is a
/// no-op, so this stays correct on the path where the net thread never started.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DrainNetwork;

/// The list screen asking to join the server it names.
///
/// **A name, not an address.** `ui` renders the list and asks by the registry's own
/// name for a server; turning that into an address and a certificate to expect is the
/// network boundary's job, and a UI that could name an address is a UI that could put
/// one on screen or into a log. The same rule [`DisconnectRequest`] follows: `ui` may
/// ask, only this module may open a socket.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub name: String,
}

/// A player asking for the server the last session was on to be dialled again.
///
/// **It carries nothing, and that is the point.** Where to dial is [`RejoinBy`], which
/// this module recorded when the session was opened and which `ui` cannot see: a route
/// is a row name or an address with the certificate to expect at it, and a request that
/// carried either would be `ui` naming a server to connect to rather than asking to go
/// back to the one it was already on. The screen knows only that there is *an* address
/// to return to, which is [`ServerAddress`] being present.
///
/// **Nothing writes one but a press.** There is no timer, no backoff and no retry
/// policy behind this message — `client/AGENTS.md` says there is none and this does not
/// add one. A client that redialled on its own would be hammering a server that had just
/// closed it, which is the one thing #627 must not turn a dead screen into.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectRequest;

/// When a session is opened, and against what.
///
/// Three shapes rather than an address and a pair of flags, because the differences
/// between them are exactly the decisions a reviewer needs to see together: which
/// certificate is expected, and whether anything is presented.
#[derive(Debug, Clone)]
enum Dial {
    /// Nothing until a [`ConnectRequest`] names a row of the list. A player's client:
    /// the row carries the address and the fingerprint to expect at it, from the same
    /// row, so there is no way to dial one without the other.
    OnRequest,
    /// At build, against `addr`, with `expected`, presenting no account.
    ///
    /// `--server` with no account service. There is no ticket on this machine to
    /// present and no service to ask for one, so the hello names no account — which a
    /// server built after #102 refuses, in as many words. It is left reachable because
    /// the refusal is the truthful answer to a launch that named nowhere to sign in,
    /// and because it is the shape the tests below drive against a stub server.
    AtBuild {
        addr: String,
        expected: tls::Expectation,
    },
    /// Once a sign-in has cached a ticket for the world this launch named, against
    /// `addr`, which is in no list.
    ///
    /// **`--server` together with `--account-service`, and it is what #154 added.** The
    /// login screen comes up first and nothing is dialled behind it; when the ticket is
    /// in hand the address the developer typed is dialled with
    /// [`tls::Expectation::Unlisted`] — encrypted, verified against nothing, and saying
    /// so — and the ticket is presented, because it is the only thing that gets in.
    WhenSignedIn { addr: String },
}

/// Owns the socket thread and publishes what it reports.
///
/// **The address is no longer a launch setting**, which is the shape change this
/// plugin carries. A client with an account service and no address starts at
/// [`ConnectionState::Idle`], with no socket and no [`ServerAddress`], and connects
/// when a [`ConnectRequest`] names a server the list carried. `--server` is the other
/// way in and it is the development path: an address that is in no list, with nothing
/// to verify it against and therefore no identity presented. See [`Dial`] for the three
/// shapes and [`tls::Expectation`] for what an address in no list means.
pub struct NetPlugin {
    /// When to open a session, and against what.
    dial: Dial,
    player_name: String,
    identity_path: Option<PathBuf>,
    /// Where per-server files are kept, when a caller names the directory. `None` in
    /// every shipped launch — see [`session::Target::data_home`].
    data_home: Option<PathBuf>,
    /// Which transport the session thread builds.
    ///
    /// Always [`session::Transport::Encrypted`] in a shipped client — the other variant
    /// does not exist outside `cfg(test)`, so this field cannot be set to anything else
    /// in a build a player runs. See [`session::Transport`].
    transport: session::Transport,
}

impl NetPlugin {
    /// Waits for a [`ConnectRequest`] naming a server out of the list.
    ///
    /// The shape a player's client is built in: nothing is dialled until somebody
    /// clicks a row, and the row carries the fingerprint the session is verified
    /// against.
    pub fn listening() -> Self {
        Self {
            dial: Dial::OnRequest,
            player_name: DEFAULT_PLAYER_NAME.to_owned(),
            identity_path: None,
            data_home: None,
            transport: session::Transport::Encrypted,
        }
    }

    /// Dials `server_addr` when the app is built, with nothing to verify it against and
    /// no account to present.
    ///
    /// **`--server` with no account service.** An address given on the command line is
    /// in no list, so nothing states which certificate to expect there: the session is
    /// encrypted and unauthenticated, and it presents no identity and stores none,
    /// which is what keeps "a stored identity is never presented to an unverified
    /// server" true.
    ///
    /// It also presents no *account*, because this launch was told of no service to get
    /// one from — and a server that admits players on a signed ticket refuses that
    /// hello. Naming an account service alongside the address is what makes this path
    /// connect; see [`Self::developing_against_signed_in`].
    pub fn developing_against(server_addr: impl Into<String>) -> Self {
        Self {
            dial: Dial::AtBuild {
                addr: server_addr.into(),
                expected: tls::Expectation::Unlisted,
            },
            ..Self::listening()
        }
    }

    /// Dials `server_addr` once a sign-in has cached a ticket, with nothing to verify it
    /// against.
    ///
    /// **`--server` together with `--account-service`, and it is the development path
    /// that can actually reach a world.** The certificate expectation is unchanged from
    /// [`Self::developing_against`] — an address in no list states none, and this
    /// session says so — but the hello now carries the world ticket the sign-in
    /// obtained, which is the only thing a server built after #102 admits anybody on.
    /// See [`session::Target::ticket`] for why presenting one here is a bounded trade
    /// where presenting a stored identity would not be.
    pub fn developing_against_signed_in(server_addr: impl Into<String>) -> Self {
        Self {
            dial: Dial::WhenSignedIn {
                addr: server_addr.into(),
            },
            ..Self::listening()
        }
    }

    /// Dials `server_addr` at build with the expectation a list row would have carried.
    ///
    /// **Test-only, and it is the shape [`connect_on_request`] produces** — an address
    /// and a fingerprint out of one row — reached without a list so the tests below can
    /// drive the whole thread-and-channel boundary against a stub server. It is what
    /// they need, because it is the variant that reads and writes the identity file;
    /// the fingerprint itself is never looked at, since these tests run over
    /// [`session::Transport::Plaintext`] and a plaintext session has no certificate.
    #[cfg(test)]
    fn as_if_listed(server_addr: impl Into<String>) -> Self {
        Self {
            dial: Dial::AtBuild {
                addr: server_addr.into(),
                expected: tls::Expectation::Listed("0".repeat(tls::FINGERPRINT_CHARS)),
            },
            ..Self::listening()
        }
    }

    /// Announces `name` in the hello instead of [`DEFAULT_PLAYER_NAME`].
    ///
    /// A display name and nothing more: untrusted, non-unique, and never what the
    /// server keys an identity on.
    pub fn with_player_name(mut self, name: impl Into<String>) -> Self {
        self.player_name = name.into();
        self
    }

    /// Drives this session over a plain socket, for the stub server the tests below
    /// stand up. See [`session::Transport`] for why the seam exists and why it cannot
    /// be reached from a shipped client.
    #[cfg(test)]
    fn over_plaintext(mut self) -> Self {
        self.transport = session::Transport::Plaintext;
        self
    }

    /// Keeps the identity in `path` rather than in the per-server file.
    ///
    /// `None` leaves the derivation to the net thread, which is the only code that
    /// knows where a token belongs; this is `--identity` and nothing else.
    pub fn with_identity_path(mut self, path: Option<PathBuf>) -> Self {
        self.identity_path = path;
        self
    }

    /// Keeps this client's per-server files under `path` rather than under the data
    /// directory the environment names.
    ///
    /// **Test-only, and it exists because one of those files is written by a *session***:
    /// the character a welcome settles on is remembered per server, so a test that drove a
    /// whole handshake would otherwise leave a file in the developer's own data directory
    /// — and a later run reading it back would make one test depend on another. The
    /// identity file has `--identity` for the same reason; this one has no flag, because
    /// nothing a player does needs to move it.
    #[cfg(test)]
    fn with_data_home(mut self, path: PathBuf) -> Self {
        self.data_home = Some(path);
        self
    }
}

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        // One value, published for the connect system and used here for the
        // development path: two constructions of the same three settings would be two
        // things to keep in step, and the one that drifted would be the one a player
        // never exercises.
        let settings = SessionSettings {
            player_name: self.player_name.clone(),
            identity_path: self.identity_path.clone(),
            data_home: self.data_home.clone(),
            transport: self.transport,
        };

        app.init_resource::<WorldInbox>()
            .init_resource::<SnapshotInbox>()
            .init_resource::<InventoryInbox>()
            .init_resource::<LearnedMountsInbox>()
            .init_resource::<LootInbox>()
            .init_resource::<VendorInbox>()
            .init_resource::<PlayerTradeInbox>()
            .init_resource::<MobHitInbox>()
            .init_resource::<MapInbox>()
            .init_resource::<MineProgressInbox>()
            .init_resource::<AppearanceInbox>()
            .init_resource::<ResidentInbox>()
            .init_resource::<RefusalInbox>()
            .init_resource::<StormInbox>()
            .init_resource::<WardsInbox>()
            .init_resource::<ChatInbox>()
            .init_resource::<SessionEndingInbox>()
            .insert_resource(settings.clone())
            .add_message::<DisconnectRequest>()
            .add_message::<CancelLeaveRequest>()
            .add_message::<ConnectRequest>()
            .add_message::<ReconnectRequest>()
            .add_message::<ChooseCharacter>()
            // Registered whether or not a session exists yet, which is the whole
            // point: the connect system is what makes one. Each of the three reads
            // the link as an `Option`, so a client with no session is a client
            // whose systems return immediately rather than one missing them.
            .add_systems(
                Update,
                (
                    // Ahead of both dials, and chained to them, so the `ConnectRequest` it
                    // writes is read in the same frame it is written: a rejoin that waited
                    // a frame would show the server list over the world the player had
                    // just left.
                    rejoin_for_a_character,
                    // Ahead of `connect_on_request` for the same reason the rejoin is:
                    // a `Row` route is answered by writing the message a click on that
                    // row writes, and it is read in the frame it is written.
                    reconnect_on_request,
                    connect_on_request,
                    connect_once_signed_in,
                    drain_session_events.in_set(DrainNetwork),
                    // After the drain, so a choice made this frame is sent against the
                    // exchange this frame's events describe rather than the last one's.
                    send_character_choice.after(DrainNetwork),
                    update_leave_countdown.after(DrainNetwork),
                    cancel_leave_on_request.after(DrainNetwork),
                    disconnect_on_request.after(DrainNetwork),
                )
                    .chain(),
            );

        let (addr, expected) = match self.dial.clone() {
            Dial::OnRequest => {
                // The list decides, and it has not been read yet. No socket, no address.
                app.insert_resource(ConnectionState::Idle);
                return;
            }
            Dial::WhenSignedIn { addr } => {
                // The login screen is what happens next, and nothing is dialled behind
                // it: a hello with no ticket is refused, so connecting first would put
                // a refusal on screen underneath a sign-in that was about to fix it.
                // `connect_once_signed_in` opens the session when the ticket is in hand.
                app.insert_resource(ConnectionState::Idle)
                    .insert_resource(DialWhenSignedIn(addr));
                return;
            }
            Dial::AtBuild { addr, expected } => (addr, expected),
        };

        // `--server` with no account service: dialled now, `Unlisted` because nothing
        // named a certificate to expect at an address somebody typed, and presenting no
        // account because this launch was told of nowhere to get one.
        match start_session(&addr, expected.clone(), &settings, None) {
            Ok((link, outbound)) => {
                app.insert_resource(ConnectionState::Connecting)
                    // How to come back here if the player leaves. Recorded even though
                    // this dial happened at build: `rejoin_for_a_character` is a system,
                    // and a system has no plugin to ask what it was built with.
                    .insert_resource(RejoinBy::Address {
                        addr: addr.clone(),
                        expected: expected.clone(),
                        ticket_path: None,
                    })
                    .insert_resource(ServerAddress(addr))
                    .insert_resource(link)
                    .insert_resource(outbound);
            }
            // Not a panic: a client that cannot start a thread can still tell the
            // player so, and "no panic, no silent exit" has no exception for
            // failures that are nobody's fault.
            Err(err) => {
                error!("the network thread would not start: {err}");
                app.insert_resource(ConnectionState::Rejected { reason: err })
                    .insert_resource(ServerAddress(addr));
            }
        }
    }
}

/// Everything a session needs that does not come from the row that was clicked.
///
/// A resource because the connect system runs long after the plugin was built, and
/// these three are the settings the launch decided rather than the list.
#[derive(Resource, Clone)]
struct SessionSettings {
    player_name: String,
    identity_path: Option<PathBuf>,
    /// Where the per-server files live when something names the directory. `None` in
    /// every shipped launch — see [`session::Target::data_home`].
    data_home: Option<PathBuf>,
    transport: session::Transport,
}

/// Starts the net thread and hands back the two ECS-side resources it needs.
///
/// One function for all three callers — the development path at build time,
/// [`connect_on_request`] when a list row is clicked, and [`connect_once_signed_in`]
/// when a sign-in has produced a ticket — so a session is started exactly one way. The
/// handle is dropped, detaching the thread: joining it would mean blocking app teardown
/// on a socket, and the command channel closing is what tells it to stop.
///
/// `ticket` is the cached file the hello presents an account from, and `None` is a
/// launch with none. **It is a path and never the bytes**: the ticket is read on the
/// session thread, which is what keeps it out of the ECS entirely — the same fence the
/// identity file sits behind. See [`session::Target::ticket`].
fn start_session(
    addr: &str,
    expected: tls::Expectation,
    settings: &SessionSettings,
    ticket: Option<PathBuf>,
) -> Result<(NetLink, Outbound), String> {
    let (event_tx, event_rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel();
    // Bounded, unlike the other two: this is the only channel the ECS *produces* into,
    // and a producer that cannot block has to be able to drop. See OUTBOUND_QUEUE.
    let (outbound_tx, outbound_rx) = mpsc::sync_channel(OUTBOUND_QUEUE);
    let session_outbound = outbound_tx.clone();

    let addr = addr.to_owned();
    let player_name = settings.player_name.clone();
    let identity_path = settings.identity_path.clone();
    let data_home = settings.data_home.clone();
    let transport = settings.transport;

    thread::Builder::new()
        .name("voxelheim-net".to_owned())
        .spawn(move || {
            session::run(
                session::Target {
                    addr,
                    expected,
                    player_name,
                    identity_override: identity_path,
                    ticket,
                    data_home,
                    transport,
                },
                event_tx,
                command_rx,
                session_outbound,
                outbound_rx,
            )
        })
        .map_err(|err| format!("cannot start the network thread: {err}"))?;

    Ok((
        NetLink(Mutex::new(Channels {
            events: event_rx,
            commands: command_tx,
        })),
        Outbound(Mutex::new(outbound_tx)),
    ))
}

/// How the live session was opened, so leaving it can open the world again.
///
/// Recorded when a connection is dialled and read when the player asks to leave. It is the
/// *route*, not the address: a row of the list carries a certificate fingerprint alongside
/// its address, and re-dialling by address alone would be verifying against nothing. Going
/// back through the path that opened the session in the first place is what keeps that
/// structural.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
enum RejoinBy {
    /// A row of the server list, by name. [`rejoin_for_a_character`] writes the same
    /// `ConnectRequest` a click writes, so the address and the expectation come out of the
    /// row a second time rather than being remembered separately from the fingerprint that
    /// verifies them.
    Row(String),
    /// A `--server` launch, which has no list and no row. Everything `start_session` needs,
    /// because there is nowhere else to look it up: both launch paths reach here, the one
    /// dialled at build and the one that waits for a ticket.
    Address {
        addr: String,
        expected: tls::Expectation,
        ticket_path: Option<PathBuf>,
    },
}

/// Set while this client is on its way back to the character screen.
///
/// Two events insert it, and both have a complete remedy on the same route:
/// `disconnect_on_request`, when the player asks to leave a world, and a
/// `CHARACTER_NAME_TAKEN` / `CHARACTER_NAME_REFUSED` that answered a creation, when the
/// player can type another name. Every other refusal, dropped connection or dead net
/// thread leaves `Rejected` or `Disconnected` with its reason and stays there.
///
/// It is removed by whichever dial consumes it, before that dial can fail. So a rejoin
/// that is itself refused reports the refusal and stops, which is what keeps this one
/// return rather than a retry policy — `client/AGENTS.md` still says there is none.
#[derive(Resource, Debug, Default)]
struct Rejoining;

/// The local presentation clock for a server-owned leave duration.
///
/// It never ends the session. It only updates the integer displayed in
/// [`ConnectionState::Leaving`]; the net thread's `Ended` event is the sole completion.
#[derive(Resource, Debug)]
struct LeaveCountdown {
    deadline: Instant,
}

/// Asks the world to be opened again, on the route the last session was opened by.
///
/// **Only after a deliberate return or a retryable character-name answer.** The trigger
/// is [`Rejoining`]; generic failures never insert it.
///
/// It waits for [`NetLink`] to be gone rather than dialling the frame the request lands:
/// the read thread is still represented by that resource until it reports its orderly end,
/// and a second session opened over the top of one still closing is two threads believing
/// they own a socket.
fn rejoin_for_a_character(
    rejoining: Option<Res<Rejoining>>,
    route: Option<Res<RejoinBy>>,
    settings: Res<SessionSettings>,
    link: Option<Res<NetLink>>,
    mut state: ResMut<ConnectionState>,
    mut requests: MessageWriter<ConnectRequest>,
    mut commands: Commands,
) {
    if rejoining.is_none() || link.is_some() {
        return;
    }
    // A rejoin already under way, or one that has arrived. Either is somebody else's
    // business now; the flag is dropped so nothing dials a second time. `Choosing` is
    // the deliberate exception: a rejected character name keeps the form mounted while
    // this one replacement connection is opened.
    if !matches!(
        *state,
        ConnectionState::Choosing
            | ConnectionState::Disconnected
            | ConnectionState::Rejected { .. }
    ) {
        commands.remove_resource::<Rejoining>();
        return;
    }

    match route.as_deref() {
        Some(RejoinBy::Row(name)) => {
            // The same message a click on that row writes, read by the same system in the
            // same frame. Leave `Rejoining` for that system to consume: its presence is
            // what lets this internal request pass while the visible state remains
            // `Choosing`, without widening ordinary list clicks to that state.
            requests.write(ConnectRequest { name: name.clone() });
        }
        Some(RejoinBy::Address {
            addr,
            expected,
            ticket_path,
        }) => {
            // **Dropped before anything can fail, which is what makes this one return
            // rather than a retry policy.** A rejoin that is itself refused leaves
            // `Rejected` with the reason and nothing set to try again.
            commands.remove_resource::<Rejoining>();
            dial_recorded_address(
                addr,
                expected,
                ticket_path.clone(),
                &settings,
                &mut state,
                &mut commands,
            );
        }
        None => {
            // Nothing was ever dialled, so there is nothing to go back to. Reachable only
            // if a `DisconnectRequest` arrived on a client that never opened a session.
            commands.remove_resource::<Rejoining>();
        }
    }
}

/// Opens a session against an address this client has already dialled once.
///
/// The one place a [`RejoinBy::Address`] route becomes a socket, shared by both systems
/// that go back to a recorded one — [`rejoin_for_a_character`] and
/// [`reconnect_on_request`] — so "there is a single dial path" stays a property of the
/// code rather than a convention two copies have to keep. [`RejoinBy::Row`] needs no
/// equivalent: a row is dialled by writing the [`ConnectRequest`] a click on it writes,
/// which is how the address and the fingerprint come out of the row together.
///
/// It does not touch [`Rejoining`]. Whether a flag was consumed is the caller's
/// bookkeeping, and the two callers answer it differently.
fn dial_recorded_address(
    addr: &str,
    expected: &tls::Expectation,
    ticket_path: Option<PathBuf>,
    settings: &SessionSettings,
    state: &mut ConnectionState,
    commands: &mut Commands<'_, '_>,
) {
    match start_session(addr, expected.clone(), settings, ticket_path) {
        Ok((link, outbound)) => {
            *state = ConnectionState::Connecting;
            commands.insert_resource(ServerAddress(addr.to_owned()));
            commands.insert_resource(link);
            commands.insert_resource(outbound);
        }
        Err(err) => {
            error!("the network thread would not start: {err}");
            *state = ConnectionState::Rejected { reason: err };
        }
    }
}

/// Dials the server the last session was on again, because a player pressed RECONNECT.
///
/// **The route is the one the session was opened by, never an address off a screen.**
/// [`RejoinBy`] carries a row's *name* where there was a list, so the address and the
/// certificate to expect at it are read out of that row a second time rather than
/// remembered apart from the fingerprint that verifies them — which is the whole reason
/// that resource holds a route instead of an address. Where there was no list it carries
/// the address together with the expectation for it.
///
/// **Only from a state a session has ended in, and only once the previous thread has
/// gone.** [`NetLink`] exists until the net thread reports its end, and a second session
/// opened over one still closing is two threads believing they own a socket — the same
/// wait [`rejoin_for_a_character`] makes for the same reason. A press arriving in any
/// other state is a press on a control the screen was not drawing.
///
/// **It inserts no [`Rejoining`].** That flag is the character screen's one internal
/// retry, consumed by the dial it arms; a press is not one, and leaving it set here
/// would arm a dial nobody asked for the *next* time a session ended. The line #184
/// drew — a dropped connection dials nothing on its own — is exactly where it was.
fn reconnect_on_request(
    mut requests: MessageReader<ReconnectRequest>,
    route: Option<Res<RejoinBy>>,
    settings: Res<SessionSettings>,
    link: Option<Res<NetLink>>,
    mut state: ResMut<ConnectionState>,
    mut connects: MessageWriter<ConnectRequest>,
    mut commands: Commands,
) {
    // The whole batch is consumed however many arrived: several presses in one frame are
    // one connection, not one replayed across later frames. The rule `connect_on_request`
    // and `disconnect_on_request` already keep.
    if requests.read().last().is_none() {
        return;
    }
    if link.is_some() {
        return;
    }
    if !matches!(
        *state,
        ConnectionState::Rejected { .. } | ConnectionState::Disconnected
    ) {
        return;
    }

    match route.as_deref() {
        Some(RejoinBy::Row(name)) => {
            connects.write(ConnectRequest { name: name.clone() });
        }
        Some(RejoinBy::Address {
            addr,
            expected,
            ticket_path,
        }) => {
            dial_recorded_address(
                addr,
                expected,
                ticket_path.clone(),
                &settings,
                &mut state,
                &mut commands,
            );
        }
        // Nothing was ever dialled, so there is nothing to come back to. The screen does
        // not offer the control without an address, and reaching here anyway costs one
        // ignored message rather than a guess at one.
        None => {}
    }
}

/// Opens a session against the server a [`ConnectRequest`] named.
///
/// **The address and the fingerprint come out of the same row**, which is what makes
/// "the certificate is verified against what the list carried" structural rather than a
/// pair of values somebody has to keep together. A request naming a server that is not
/// in the list this client holds is refused rather than guessed at: there is nothing to
/// verify such a server against, and a name is not an address.
fn connect_on_request(
    mut requests: MessageReader<ConnectRequest>,
    list: Option<Res<ServerList>>,
    rejoining: Option<Res<Rejoining>>,
    settings: Res<SessionSettings>,
    mut state: ResMut<ConnectionState>,
    mut commands: Commands,
) {
    // The last of the batch wins, and the whole batch is consumed: several clicks in
    // one frame are one connection, not one replayed across later frames. The same
    // rule `disconnect_on_request` and `start_sign_in` keep.
    let Some(request) = requests.read().last().cloned() else {
        return;
    };

    // Only from a standing start. A click that arrives while a session is live or
    // being opened is a click the list screen was not showing a button for.
    let reopening_character = rejoining.is_some() && matches!(*state, ConnectionState::Choosing);
    if !reopening_character
        && !matches!(
            *state,
            ConnectionState::Idle
                | ConnectionState::Rejected { .. }
                | ConnectionState::Disconnected
        )
    {
        return;
    }
    if reopening_character {
        // Consume the single internal retry before the dial can fail. Ordinary clicks
        // cannot reach `Choosing`, and a failed retry has no flag left to loop on.
        commands.remove_resource::<Rejoining>();
    }

    let chosen = match list.as_deref() {
        Some(ServerList::Ready(servers)) => {
            servers.iter().find(|server| server.name() == request.name)
        }
        _ => None,
    };
    let Some(chosen) = chosen else {
        // Reachable only if the list changed under the screen between the click and
        // this system. Reported rather than ignored: a button that does nothing is the
        // one outcome a player cannot act on.
        warn!("a server was chosen that is not in the list this client holds");
        *state = ConnectionState::Rejected {
            reason: "that server is no longer in the list. Refresh it and try again.".to_owned(),
        };
        return;
    };

    // `None` for the ticket, and it stays `None` here until #107. Signing in caches an
    // *account* ticket — one that names no world — and a game server refuses one of
    // those with `ErrWrongWorld`, correctly: joining needs a ticket for the world being
    // joined, and on this path choosing the world is the list's job rather than the
    // command line's. #154 gave the development path a world to name; this one has a
    // row that already carries the name and nothing yet that asks for a ticket in it.
    match start_session(chosen.address(), chosen.expectation(), &settings, None) {
        Ok((link, outbound)) => {
            // Set here rather than through `Commands`, so the state is already
            // `Connecting` by the time the link it describes exists.
            *state = ConnectionState::Connecting;
            // How to come back here if the player leaves. The name rather than the
            // address, so the row's fingerprint is found again with it.
            commands.insert_resource(RejoinBy::Row(request.name.clone()));
            commands.insert_resource(ServerAddress(chosen.address().to_owned()));
            commands.insert_resource(link);
            commands.insert_resource(outbound);
        }
        Err(err) => {
            error!("the network thread would not start: {err}");
            *state = ConnectionState::Rejected { reason: err };
        }
    }
}

/// The address a [`Dial::WhenSignedIn`] launch dials once it has a ticket.
///
/// Present exactly on that path, which is what makes [`connect_once_signed_in`] a no-op
/// everywhere else rather than a system with a flag to read. **It holds an address and
/// nothing else**: no certificate, because an address in no list states none, and no
/// ticket, because a ticket never reaches the ECS.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
struct DialWhenSignedIn(String);

/// Opens the one session a `--server` launch with an account service asks for, as soon
/// as the sign-in has a ticket to present.
///
/// **The trigger is the sign-in rather than a click, because there is nothing to click.**
/// This path has no list and no rows: the address was decided at launch, so the only
/// thing still missing when the app starts is the credential. The login screen is up
/// until it arrives, and this dials underneath it the frame it does.
///
/// **Only from [`ConnectionState::Idle`], and that is what makes it happen once.** A
/// session that ends leaves `Disconnected`, and one that is refused leaves `Rejected` —
/// neither of which this reads, so a refusal is a refusal a player can read rather than
/// a redial loop against a server that has already said no. [`connect_on_request`]
/// accepts those two states because the list screen offers a way to ask again; this path
/// offers none, and inventing one here would be inventing a retry policy.
fn connect_once_signed_in(
    dial: Option<Res<DialWhenSignedIn>>,
    sign_in: Option<Res<SignInState>>,
    sign_in_settings: Option<Res<SignInSettings>>,
    settings: Res<SessionSettings>,
    mut state: ResMut<ConnectionState>,
    mut commands: Commands,
) {
    let Some(dial) = dial else {
        return;
    };
    if !matches!(*state, ConnectionState::Idle) {
        return;
    }
    // Absent on a client with no account service, which cannot be this path — and
    // absent in a test that built `NetPlugin` on its own. Either way there is nothing
    // to sign in with, so there is nothing to do and nothing to claim.
    if sign_in.as_deref() != Some(&SignInState::SignedIn) {
        return;
    }
    let Some(sign_in_settings) = sign_in_settings else {
        return;
    };

    // The path, never the ticket. `SignInPlugin` derived it for the world this launch
    // named, so the file it points at holds a ticket for that world and no other — see
    // `tickets::world_ticket_path`. `None` is a launch that could name no file to keep
    // a sign-in in, which the session thread reports as the refusal it is.
    match start_session(
        &dial.0,
        tls::Expectation::Unlisted,
        &settings,
        sign_in_settings.ticket_path.clone(),
    ) {
        Ok((link, outbound)) => {
            // Set here rather than through `Commands`, so the state is already
            // `Connecting` by the time the link it describes exists — which is also
            // what stops this system starting a second one on the next frame.
            *state = ConnectionState::Connecting;
            // How to come back here if the player leaves. There is no list and no row on
            // this path, so the rejoin dials it directly.
            commands.insert_resource(RejoinBy::Address {
                addr: dial.0.clone(),
                expected: tls::Expectation::Unlisted,
                ticket_path: sign_in_settings.ticket_path.clone(),
            });
            commands.insert_resource(ServerAddress(dial.0.clone()));
            commands.insert_resource(link);
            commands.insert_resource(outbound);
        }
        Err(err) => {
            error!("the network thread would not start: {err}");
            *state = ConnectionState::Rejected { reason: err };
        }
    }
}

/// The ECS end of the net thread's channels.
///
/// The `Mutex` is a type obligation rather than a synchronisation one: a Bevy
/// resource must be `Sync`, and both `std::sync::mpsc` endpoints are `Send`-only.
/// Nothing contends it — the only accessor is [`drain_session_events`], which
/// takes the resource mutably and so reaches the contents through `get_mut`
/// without ever locking.
#[derive(Resource)]
struct NetLink(Mutex<Channels>);

struct Channels {
    events: Receiver<SessionEvent>,
    commands: Sender<NetCommand>,
}

impl Drop for Channels {
    /// Dropping the ECS end is how the app says "stop".
    ///
    /// Sending is belt to the braces of the channel closing behind it: the
    /// explicit command is noticed on the net thread's next poll, and a dropped
    /// `Sender` says the same thing to a thread that manages to look between the
    /// two. Failure means the thread is already gone, which is the state being
    /// asked for.
    fn drop(&mut self) {
        let _ = self.commands.send(NetCommand::Disconnect);
    }
}

/// Every queue the net thread fills, as one parameter.
///
/// Grouped rather than listed, because there is one of these per server→client payload
/// this client consumes and the list only grows. A `SystemParam` is the shape Bevy already
/// has for that — the same one `player::combat::HeldItem` uses — and it keeps
/// [`drain_session_events`] a signature somebody can read.
#[derive(bevy::ecs::system::SystemParam)]
struct Inboxes<'w> {
    world: ResMut<'w, WorldInbox>,
    snapshots: ResMut<'w, SnapshotInbox>,
    inventories: ResMut<'w, InventoryInbox>,
    learned_mounts: ResMut<'w, LearnedMountsInbox>,
    // Optional only for focused boundary tests that install the drain directly.
    loot: Option<ResMut<'w, LootInbox>>,
    // Optional only for focused boundary tests that install the drain directly.
    mob_hits: Option<ResMut<'w, MobHitInbox>>,
    // Optional only for focused boundary tests that install the drain directly.
    vendor: Option<ResMut<'w, VendorInbox>>,
    // Optional only for focused boundary tests that install the drain directly.
    player_trade: Option<ResMut<'w, PlayerTradeInbox>>,
    // Optional only for focused boundary tests that install the drain directly.
    map: Option<ResMut<'w, MapInbox>>,
    mining: ResMut<'w, MineProgressInbox>,
    appearances: ResMut<'w, AppearanceInbox>,
    residents: ResMut<'w, ResidentInbox>,
    refusals: ResMut<'w, RefusalInbox>,
    storms: ResMut<'w, StormInbox>,
    wards: ResMut<'w, WardsInbox>,
    // Optional only for focused net-boundary tests that install the drain directly.
    // NetPlugin always initialises it, so a live client never drops this queue.
    chat: Option<ResMut<'w, ChatInbox>>,
    endings: ResMut<'w, SessionEndingInbox>,
}

/// Resources that exist only while an established session is suspended for leave.
#[derive(bevy::ecs::system::SystemParam)]
struct LeavingSession<'w> {
    cancellation: Option<ResMut<'w, LeaveCancellation>>,
    suspended: Option<ResMut<'w, SuspendedOutbound>>,
}

/// Applies everything the net thread has said since the last frame.
///
/// `try_recv` in a loop, never a blocking receive: this system runs on Bevy's
/// schedule and must return whether the network had anything to say or not.
fn drain_session_events(
    mut commands: Commands,
    link: Option<ResMut<NetLink>>,
    rejoining: Option<Res<Rejoining>>,
    mut state: ResMut<ConnectionState>,
    mut leaving: LeavingSession<'_>,
    mut inboxes: Inboxes<'_>,
    mut pending_character: Option<ResMut<CharacterChoice>>,
) {
    // Absent until a server has been chosen, which is the ordinary state of a client
    // sitting on the login screen or the server list. Nothing to drain, and nothing
    // about that is a failure.
    let Some(mut link) = link else {
        return;
    };
    // `get_mut` rather than `lock`: `ResMut` is already exclusive, so there is no
    // lock to take. Poisoning is recovered from rather than propagated — nothing
    // here panics while holding it, and a client that stopped reading its socket
    // because of an unrelated panic elsewhere would be a worse outcome than a
    // recovered mutex.
    let channels = match link.0.get_mut() {
        Ok(channels) => channels,
        Err(poisoned) => poisoned.into_inner(),
    };
    // A reject and the orderly close behind it may arrive in one drain or in two
    // frames. The resource covers the latter; the local flag is set immediately for
    // the former, before deferred commands can make the resource visible.
    let mut reopening_character = rejoining.is_some()
        && pending_character
            .as_deref()
            .is_some_and(|choice| choice.creation_refusal.is_some());

    loop {
        match channels.events.try_recv() {
            Ok(SessionEvent::Handshaking) => *state = ConnectionState::Handshaking,

            Ok(SessionEvent::Characters {
                list,
                played_before,
            }) => {
                // The remembered id is matched against the list here rather than on the
                // screen, because "still in the list" is the only thing that makes it
                // worth anything — a character on another account, or a file from a world
                // that has moved on, preselects nothing rather than a row that is not
                // there.
                let preselect = played_before
                    .filter(|id| list.characters.iter().any(|c| c.character_id == *id));
                info!(
                    "the server is waiting for a character: {} of at most {}",
                    list.characters.len(),
                    list.max_characters
                );
                let creation_refusal = pending_character
                    .as_deref()
                    .and_then(|choice| choice.creation_refusal.clone());
                commands.insert_resource(CharacterChoice {
                    characters: list.characters,
                    max_characters: list.max_characters,
                    preselect,
                    answered: false,
                    attempted: None,
                    // A reconnect after a name refusal is the same form answering the
                    // same player action, not a fresh screen. Preserve the server's
                    // sentence while replacing every server-owned list field with the
                    // new connection's answer.
                    creation_refusal,
                });

                *state = ConnectionState::Choosing;
            }

            Ok(SessionEvent::Established { params, returning }) => {
                // This queue outlives a socket so the plugins can share one resource.
                // A newly established session must not inherit an unread ward answer
                // from the connection it replaced; answers later in this same ordered
                // drain belong to the new session and are queued normally.
                inboxes.wards.clear();
                inboxes.learned_mounts.clear();
                // Every field but the token, which is never written down. The
                // newtype refuses to print itself, so this stays true even if a
                // later line reaches for `{params:?}`.
                let identity = match returning {
                    Some(true) => Identity::Returning,
                    Some(false) => Identity::New,
                    None => Identity::Untold,
                };
                info!(
                    "session established: entity {} at {:?}, seed {}, {} Hz, chunk {}, view {}, {}",
                    params.entity_id,
                    params.spawn,
                    params.world_seed,
                    params.tick_rate,
                    params.chunk_size,
                    params.view_distance,
                    match identity {
                        Identity::Returning => "a returning character",
                        Identity::New => "a new character",
                        // Not "a new character", which this session has no way to
                        // know and the server may well contradict. See
                        // `session::SessionEvent::Established`.
                        Identity::Untold => "the character this account left here",
                    },
                );
                commands.insert_resource(Session(params));
                commands.insert_resource(identity);
                // The exchange is over: this session has a character. Removing it is
                // what takes the screen down, the same way inserting it put one up.
                commands.remove_resource::<CharacterChoice>();
                *state = ConnectionState::Connected;
            }

            // The net thread has no logger of its own; this is how it gets one.
            // Never carries a token — see `SessionEvent::Warning`.
            Ok(SessionEvent::Warning(detail)) => warn!("{detail}"),

            // Queued, not logged: the server sends hundreds of these on join, and
            // a line each would bury everything else in the log. The world
            // module's counters are the visible signal that they are arriving.
            Ok(SessionEvent::World(update)) => inboxes.world.0.push(update),

            // Queued, not logged, for the same reason as a chunk: there are twenty of
            // these a second. The player module's counters are the visible signal.
            Ok(SessionEvent::Snapshot { snapshot, at }) => inboxes.snapshots.0.push((snapshot, at)),

            // Complete state, queued for the player module rather than interpreted here.
            Ok(SessionEvent::Inventory(inventory)) => inboxes.inventories.0.push(inventory),
            Ok(SessionEvent::LearnedMounts(mounts)) => inboxes.learned_mounts.0.push(mounts),

            Ok(SessionEvent::LootState(state)) => {
                if let Some(loot) = inboxes.loot.as_deref_mut() {
                    loot.0.push(LootEvent::State(state));
                }
            }
            Ok(SessionEvent::LootClosed(closed)) => {
                if let Some(loot) = inboxes.loot.as_deref_mut() {
                    loot.0.push(LootEvent::Closed(closed));
                }
            }
            Ok(SessionEvent::MobHit(hit)) => {
                if let Some(mob_hits) = inboxes.mob_hits.as_deref_mut() {
                    mob_hits.push_bounded(hit);
                }
            }

            // Queued, not logged: a screenful of tiles is a burst of these, and the
            // ledger arrives in pages. The map screen's cache is the visible signal.
            Ok(SessionEvent::MapTile(tile)) => {
                if let Some(map) = inboxes.map.as_deref_mut() {
                    map.push_bounded(MapEvent::Tile(tile));
                }
            }
            Ok(SessionEvent::MapExplored(explored)) => {
                if let Some(map) = inboxes.map.as_deref_mut() {
                    map.push_bounded(MapEvent::Explored(explored));
                }
            }
            // Not logged either, and it is the one of the three that could have been: a
            // list arrives on join and once per accepted mark, which is rare enough for a
            // line. It is queued silently anyway, because the marks on the screen are the
            // signal a player reads and a second one in the log would only be a place for
            // the two to disagree.
            Ok(SessionEvent::MarkerList(list)) => {
                if let Some(map) = inboxes.map.as_deref_mut() {
                    map.push_bounded(MapEvent::Markers(list));
                }
            }

            // Queued for the player module, which is the only thing that knows whether
            // there is a body to put a name over yet. Not logged, for the reason the map's
            // three are not: a resident entering view is as ordinary as a tile arriving,
            // and a line per one would be noise from the moment the first village exists.
            Ok(SessionEvent::ResidentAppearance(resident)) => inboxes.residents.0.push(resident),

            // V25's two vendor payloads, queued for the player module the way loot's pair
            // is. Not logged: a price list arrives once per stall a player addresses, and
            // the window is the signal.
            //
            // **The validation happens one layer down and is the reason these arrive
            // whole.** An unknown role or a duplicate price ends the session at the decode
            // boundary, so nothing that reaches this arm needs checking here.
            Ok(SessionEvent::VendorState(state)) => {
                if let Some(vendor) = inboxes.vendor.as_deref_mut() {
                    vendor.0.push(VendorEvent::State(state));
                }
            }
            Ok(SessionEvent::VendorClosed(closed)) => {
                if let Some(vendor) = inboxes.vendor.as_deref_mut() {
                    vendor.0.push(VendorEvent::Closed(closed));
                }
            }
            Ok(SessionEvent::PlayerTradeState(state)) => {
                if let Some(player_trade) = inboxes.player_trade.as_deref_mut() {
                    player_trade.0.push(PlayerTradeEvent::State(state));
                }
            }
            Ok(SessionEvent::PlayerTradeClosed(closed)) => {
                if let Some(player_trade) = inboxes.player_trade.as_deref_mut() {
                    player_trade.0.push(PlayerTradeEvent::Closed(closed));
                }
            }

            // V26's two Fimbulvetr payloads: fully decoded and fully validated one layer
            // down. Neither is logged here: a warning is visible in the HUD and chat, and
            // a ward set can arrive whenever the player walks.
            //
            // **The validation is the point of carrying them this far.** An unnameable
            // storm phase, a passed storm that still counts down, a ward list past the
            // contract's bound or a column named twice all end the session at the decode
            // boundary — which is where they should, and that is true now rather than
            // when somebody writes the renderer.
            Ok(SessionEvent::StormWarning { warning, at }) => {
                inboxes.storms.0.push((warning, at));
            }
            Ok(SessionEvent::WardsNearby(wards)) => inboxes.wards.0.push(wards),

            // Complete authoritative progress, interpreted only by the player module.
            Ok(SessionEvent::MineProgress(progress)) => inboxes.mining.0.push(progress),

            // Queued for the player module, which is the only thing that knows whether
            // there is a body to put it on yet. Not logged: one arrives per player per
            // time they enter this session's view cube.
            Ok(SessionEvent::Appearance(appearance)) => inboxes.appearances.0.push(appearance),

            // Queued for the UI rather than interpreted here. Not logged either: the one
            // half worth a log line is a refusal that says *this build* sent something the
            // server could not read, and the status line is where that decision is made,
            // beside the sentence it writes for the other half.
            Ok(SessionEvent::ActionRefused(refused)) => inboxes.refusals.0.push(refused),

            // Presentation-only queues. The UI keeps every line and never reinterprets
            // received text as a command or as identity.
            Ok(SessionEvent::Chat(message)) => {
                if let Some(chat) = inboxes.chat.as_deref_mut() {
                    chat.0.push(ChatEntry::Message(message));
                }
            }
            Ok(SessionEvent::PartyInvite(invite)) => {
                if let Some(chat) = inboxes.chat.as_deref_mut() {
                    chat.0.push(ChatEntry::PartyInvite(invite));
                }
            }

            Ok(SessionEvent::Leaving(started)) => {
                let duration = Duration::from_millis(u64::from(started.remaining_ms));
                commands.insert_resource(LeaveCountdown {
                    deadline: Instant::now() + duration,
                });
                *state = ConnectionState::Leaving {
                    seconds_remaining: Some(
                        u32::try_from(duration.as_millis().div_ceil(1_000)).unwrap_or(u32::MAX),
                    ),
                };
                if leaving.cancellation.is_none() {
                    commands.insert_resource(LeaveCancellation::Available);
                }
            }

            Ok(SessionEvent::LeaveCancellation(result)) => {
                if result.accepted {
                    let Some(suspended) = leaving.suspended.as_deref_mut() else {
                        warn!(
                            "leave cancellation was accepted without a suspended gameplay sender"
                        );
                        let _ = channels.commands.send(NetCommand::Disconnect);
                        *state = ConnectionState::Disconnected;
                        commands.remove_resource::<Outbound>();
                        commands.remove_resource::<Session>();
                        commands.remove_resource::<Identity>();
                        commands.remove_resource::<CharacterChoice>();
                        commands.remove_resource::<LeaveCountdown>();
                        commands.remove_resource::<LeaveCancellation>();
                        commands.remove_resource::<SuspendedOutbound>();
                        commands.remove_resource::<Rejoining>();
                        inboxes.wards.clear();
                        break;
                    };
                    commands.insert_resource(suspended.0.sibling());
                    *state = ConnectionState::Connected;
                    commands.remove_resource::<SuspendedOutbound>();
                    commands.remove_resource::<LeaveCancellation>();
                    commands.remove_resource::<LeaveCountdown>();
                    commands.remove_resource::<Rejoining>();
                } else {
                    let duration = Duration::from_millis(u64::from(result.remaining_ms));
                    commands.insert_resource(LeaveCountdown {
                        deadline: Instant::now() + duration,
                    });
                    *state = ConnectionState::Leaving {
                        seconds_remaining: Some(
                            u32::try_from(duration.as_millis().div_ceil(1_000)).unwrap_or(u32::MAX),
                        ),
                    };
                    if let Some(cancellation) = leaving.cancellation.as_deref_mut() {
                        *cancellation = LeaveCancellation::Refused;
                    } else {
                        commands.insert_resource(LeaveCancellation::Refused);
                    }
                }
            }

            Ok(SessionEvent::ServerRefused(reject)) => {
                let reason = reject.describe();
                let retryable_creation = reject.is_character_name_refusal()
                    && pending_character
                        .as_deref()
                        .is_some_and(|choice| choice.attempted == Some(CharacterAttempt::Create));

                warn!("no session: {reason}");
                // The server closes after every ServerReject. For the two name answers,
                // that means another connection on the same route; the cached ticket is
                // read again by the ordinary session thread and may independently fail.
                if retryable_creation {
                    if let Some(choice) = pending_character.as_deref_mut() {
                        choice.creation_refusal = Some(reason);
                    }
                    // The screen is keyed on `CharacterChoice`, and the state stays on
                    // the same player task instead of flashing a terminal rejection.
                    *state = ConnectionState::Choosing;
                    reopening_character = true;
                    commands.insert_resource(Rejoining);
                } else {
                    *state = ConnectionState::Rejected { reason };
                    commands.remove_resource::<CharacterChoice>();
                }
                commands.remove_resource::<Outbound>();
                commands.remove_resource::<Session>();
                commands.remove_resource::<Identity>();
                commands.remove_resource::<LeaveCountdown>();
                commands.remove_resource::<LeaveCancellation>();
                commands.remove_resource::<SuspendedOutbound>();
                inboxes.wards.clear();
            }

            Ok(SessionEvent::Refused(reason)) => {
                warn!("no session: {reason}");
                *state = ConnectionState::Rejected { reason };
                // Dropping the sender closes the channel, which is how the writer thread
                // learns there is nothing left to write and lets go of its socket handle.
                commands.remove_resource::<Outbound>();
                commands.remove_resource::<Session>();
                commands.remove_resource::<Identity>();
                commands.remove_resource::<CharacterChoice>();
                commands.remove_resource::<LeaveCountdown>();
                commands.remove_resource::<LeaveCancellation>();
                commands.remove_resource::<SuspendedOutbound>();
                inboxes.wards.clear();
            }

            Ok(SessionEvent::Ended(detail)) => {
                // A game the player was actually inside, ending for a reason beyond an
                // ordinary leave. The ordinary leave carries no detail (`Ended(None)`,
                // see `peer_closed`), so only the detailed case is worth a chat line —
                // and only when there was a game on screen to interrupt: a character
                // still being chosen has no established session and its own screen
                // already says "that session ended".
                let interrupted_a_game = detail.is_some()
                    && matches!(
                        *state,
                        ConnectionState::Connected | ConnectionState::Leaving { .. }
                    );
                match detail {
                    Some(detail) => warn!("session ended: {detail}"),
                    None => info!("the server closed the connection"),
                }
                if interrupted_a_game {
                    inboxes.endings.push();
                }
                if !reopening_character {
                    *state = ConnectionState::Disconnected;
                    commands.remove_resource::<CharacterChoice>();
                }
                commands.remove_resource::<Outbound>();
                commands.remove_resource::<Session>();
                commands.remove_resource::<Identity>();
                commands.remove_resource::<LeaveCountdown>();
                commands.remove_resource::<LeaveCancellation>();
                commands.remove_resource::<SuspendedOutbound>();
                inboxes.wards.clear();
            }

            Err(TryRecvError::Empty) => break,

            Err(TryRecvError::Disconnected) => {
                // The net thread is gone. It normally reports why before it goes,
                // and that report stands — but it can also vanish without one (a
                // panic, or any exit that never reaches its `Ended`), and then a
                // closed channel is the only notice the ECS gets. So the
                // correction has to cover *every* non-terminal state, `Connected`
                // included: a session whose thread has died is not a session, and
                // leaving it as `Connected` would have the status line claim a
                // live connection that no longer exists.
                //
                // The test is negative — "not already terminal" — rather than a
                // list of the mid-flight states, for two reasons. It is exhaustive
                // by construction, so a variant added to the enum later is
                // corrected by default instead of silently sailing past. And
                // excluding `Disconnected` is what makes this idempotent: `*state
                // = ...` marks the resource changed on every `DerefMut`, and this
                // arm is reached on every frame for the rest of the app's life, so
                // a guard that stayed true would re-run every consumer with a
                // change-detection filter forever. `Rejected` is excluded for a
                // second reason on top of that — it carries the reason the player
                // is reading, and overwriting it with a bare `Disconnected` would
                // throw that away. `Idle` joins them, and is the one addition that
                // is not about a session that ended: a link cannot exist while no
                // server has been chosen, so reaching here in that state would mean
                // reporting a disconnection from a server nobody dialled.
                if !reopening_character
                    && !matches!(
                        *state,
                        ConnectionState::Idle
                            | ConnectionState::Rejected { .. }
                            | ConnectionState::Disconnected
                    )
                {
                    *state = ConnectionState::Disconnected;
                    // Inside the guard, so this stays idempotent along with the
                    // assignment: `remove_resource` on an absent resource is a no-op, but
                    // queuing one every frame for the rest of the app's life is not free.
                    commands.remove_resource::<Outbound>();
                    commands.remove_resource::<Session>();
                    commands.remove_resource::<Identity>();
                    commands.remove_resource::<CharacterChoice>();
                    commands.remove_resource::<LeaveCountdown>();
                    commands.remove_resource::<LeaveCancellation>();
                    commands.remove_resource::<SuspendedOutbound>();
                }
                // **And the link itself, which used to outlive the thread it represents.**
                // Reaching this arm means the sender was dropped, and the sender lives in
                // the net thread — so there is no thread left for "the ECS end of the net
                // thread's channels" to be an end of. Nothing needed it gone before #184:
                // every reader takes it as an `Option` and a dead channel simply answered
                // `Disconnected` for ever. `rejoin_for_a_character` needs it, because
                // absence is how it knows the previous session has finished letting go of
                // its socket — and it is what makes this system stop running at all rather
                // than reach this arm on every frame for the rest of the app's life.
                //
                // Outside the guard above deliberately: that guard is about not rewriting a
                // terminal *state*, and a link with no thread is stale whichever state the
                // client is in.
                inboxes.wards.clear();
                commands.remove_resource::<NetLink>();
                break;
            }
        }
    }
}

/// Hands the player's choice of character to the net thread.
///
/// **Down the command channel rather than the outbound one**, which is what keeps a
/// single writer on that socket through the handshake: the session thread writes the
/// hello, then this, and starts the writer thread when the welcome arrives. It is also
/// what lets the handshake state machine be told that a choice went out — see
/// `handshake::Handshake::chose`.
///
/// The whole frame's batch is consumed and the last one wins, the rule every other
/// request system here keeps: two clicks in one frame are one choice, not one replayed
/// on the next frame.
fn send_character_choice(
    mut choices: MessageReader<ChooseCharacter>,
    link: Option<ResMut<NetLink>>,
    pending: Option<ResMut<CharacterChoice>>,
) {
    let Some(choice) = choices.read().last().cloned() else {
        return;
    };
    // Only while the server is actually waiting for one, and only once. A choice
    // arriving at any other moment is a screen that outlived its exchange; a *second*
    // one arrives on a session the server has already welcomed, where it is a protocol
    // error that closes the connection. See [`CharacterChoice::answered`].
    let Some(mut pending) = pending.filter(|pending| !pending.answered) else {
        return;
    };

    // No link is no session; the messages are still consumed above, so nothing replays
    // into a session opened later.
    let Some(mut link) = link else {
        return;
    };

    let channels = match link.0.get_mut() {
        Ok(channels) => channels,
        Err(poisoned) => poisoned.into_inner(),
    };
    let choice = match choice {
        ChooseCharacter::Play(character) => {
            pending.attempted = Some(CharacterAttempt::Play);
            Choice::Play(character)
        }
        ChooseCharacter::Create { name, appearance } => {
            pending.attempted = Some(CharacterAttempt::Create);
            Choice::Create(codec::CreateCharacterRequest { name, appearance })
        }
    };
    // A second submission supersedes the sentence about the first. The server remains
    // the only judge: clearing display state does not accept the new name locally.
    pending.creation_refusal = None;
    // Which of the two went out, and deliberately not who: a character's name is player
    // text, and the one thing a log needs to say here is which request the session is
    // now waiting on an answer to. The screen shows the rest, including a refusal.
    match &choice {
        Choice::Play(_) => debug!("asking to play a character this account already has"),
        Choice::Create(_) => debug!("asking to create a character"),
    }
    // A closed channel means the thread has already gone, which
    // `drain_session_events` is about to report. There is nothing to say about it here.
    let _ = channels.commands.send(NetCommand::Choose(choice));
    pending.answered = true;
}

/// Sends one cancellation intent and records only that the answer is outstanding.
///
/// Updating this resource is presentation, not resumption: [`ConnectionState::Connected`]
/// is restored exclusively by [`SessionEvent::LeaveCancellation`] carrying
/// `accepted=true`.
fn cancel_leave_on_request(
    mut requests: MessageReader<CancelLeaveRequest>,
    link: Option<ResMut<NetLink>>,
    state: Res<ConnectionState>,
    cancellation: Option<ResMut<LeaveCancellation>>,
) {
    if requests.read().count() == 0 {
        return;
    }
    if !matches!(*state, ConnectionState::Leaving { .. }) {
        return;
    }
    let Some(mut cancellation) = cancellation else {
        return;
    };
    if *cancellation == LeaveCancellation::Pending {
        return;
    }
    let Some(mut link) = link else {
        return;
    };
    let channels = match link.0.get_mut() {
        Ok(channels) => channels,
        Err(poisoned) => poisoned.into_inner(),
    };
    if channels.commands.send(NetCommand::CancelLeave).is_ok() {
        *cancellation = LeaveCancellation::Pending;
    }
}

/// Ends the live session without closing the app.
///
/// The command channel is still owned here at the network boundary. UI code can ask, but
/// cannot touch a socket or pretend the connection ended by editing display state alone.
fn disconnect_on_request(
    mut requests: MessageReader<DisconnectRequest>,
    mut commands: Commands,
    link: Option<ResMut<NetLink>>,
    mut outbound: Option<ResMut<Outbound>>,
    mut state: ResMut<ConnectionState>,
) {
    // Consume the whole frame's batch. Several UI producers asking to disconnect still
    // mean one network command, not one command replayed across several later frames.
    if requests.read().count() == 0 {
        return;
    }

    // A second disconnect is not cancellation and must not erase the countdown the
    // server already supplied. The other two states are terminal too; consuming the
    // message above is all there is to do for any of the three.
    if matches!(
        *state,
        ConnectionState::Leaving { .. }
            | ConnectionState::Rejected { .. }
            | ConnectionState::Disconnected
    ) {
        return;
    }

    // No link is no session, which is the state being asked for. The messages are
    // still consumed above, so nothing replays into a session opened later.
    let Some(mut link) = link else {
        return;
    };

    let channels = match link.0.get_mut() {
        Ok(channels) => channels,
        Err(poisoned) => poisoned.into_inner(),
    };
    // Only a welcomed session has a character for the authoritative linger to keep in
    // the world. Before that, `Leave` has no legal wire meaning and the net thread must
    // be stopped directly instead of silently dropping the command.
    let established = matches!(*state, ConnectionState::Connected);
    let command = if established {
        NetCommand::Leave
    } else {
        NetCommand::Disconnect
    };
    if channels.commands.send(command).is_err() {
        return;
    }

    // **The one place a return is asked for.** Reaching this system means a
    // `DisconnectRequest` was written, which is a player pressing something — so leaving a
    // world lands back on its character screen rather than at a dead end. Every way a
    // session ends *without* being asked to reaches `drain_session_events` instead, which
    // sets no flag: a refusal and a dropped connection are reported and stay reported.
    commands.insert_resource(Rejoining);

    commands.remove_resource::<CharacterChoice>();
    if established {
        if let Some(outbound) = outbound.as_deref_mut() {
            commands.insert_resource(SuspendedOutbound(outbound.sibling()));
        }
        commands.remove_resource::<Outbound>();
        commands.insert_resource(LeaveCancellation::Available);
        *state = ConnectionState::Leaving {
            seconds_remaining: None,
        };
    } else {
        *state = ConnectionState::Disconnected;
        commands.remove_resource::<Session>();
        commands.remove_resource::<Identity>();
        commands.remove_resource::<LeaveCountdown>();
        commands.remove_resource::<LeaveCancellation>();
        commands.remove_resource::<SuspendedOutbound>();
    }
}

/// Advances only the displayed whole-second value of an authoritative leave.
fn update_leave_countdown(
    countdown: Option<Res<LeaveCountdown>>,
    mut state: ResMut<ConnectionState>,
) {
    let Some(countdown) = countdown else {
        return;
    };
    let millis = countdown
        .deadline
        .saturating_duration_since(Instant::now())
        .as_millis();
    let remaining = u32::try_from(millis.div_ceil(1_000)).unwrap_or(u32::MAX);
    if let ConnectionState::Leaving {
        seconds_remaining: Some(seconds_remaining),
        ..
    } = &mut *state
        && *seconds_remaining != remaining
    {
        *seconds_remaining = remaining;
    }
}

/// Where a sign-in has got to, and the only thing about one that leaves this
/// module.
///
/// **The ticket itself deliberately never reaches the ECS.** It lives for the
/// length of one attempt on the sign-in thread and then only in the cache, at mode
/// `0600` — so there is no resource holding a bearer credential for a `{:?}`
/// somewhere to find, and no name outside `net` that could start deciding from
/// one. The one thing that presents a ticket is the server list read, and it happens
/// on its own thread which reads the cache — exactly as a session reads the identity
/// file.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub enum SignInState {
    /// There is no live ticket. `reason` is a line for the player when something
    /// specific happened — an expiry, a refusal — and `None` when nothing has
    /// happened yet, which is a first launch.
    SignedOut { reason: Option<String> },
    /// A browser tab is open and this client is waiting on its loopback listener.
    Waiting,
    /// A live ticket is held, so nothing asks the player for anything.
    SignedIn,
}

/// What the login screen shows when the cache held a ticket that had run out.
///
/// A sentence rather than a date, because rendering one would mean this client
/// carrying a calendar to print with — and *when* it expired is not what the player
/// needs to know, only that it did and what to do about it.
const TICKET_EXPIRED: &str = "Your last sign-in has expired. Sign in again to play.";

/// The login screen asking for a sign-in.
///
/// A message rather than a direct call, for the reason [`DisconnectRequest`] is
/// one: `ui` may ask, and only the network boundary may open a socket.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignInRequest;

/// Everything an attempt needs, kept because there can be more than one of them:
/// a refused sign-in leaves the player on the login screen with the control still
/// live.
#[derive(Resource, Clone)]
struct SignInSettings {
    service: signin::AccountService,
    /// Which world a sign-in asks for a ticket for, and `None` for an account ticket
    /// that names none. See [`SignInPlugin::for_world`].
    world: Option<String>,
    /// `None` when no file could be named to keep a ticket in. A sign-in still
    /// works; it is simply forgotten at the end of the launch.
    ticket_path: Option<PathBuf>,
    browser: signin::Browser,
}

/// The ECS end of a running attempt's channels.
///
/// Present exactly while there is a sign-in thread, which is what makes "is one
/// already running" a question about a resource rather than a flag somebody has to
/// remember to clear. The `Mutex` is the type obligation [`NetLink`] documents and
/// nothing ever contends it.
#[derive(Resource)]
struct SignInLink(Mutex<SignInChannels>);

struct SignInChannels {
    events: Receiver<SignInEvent>,
    commands: Sender<SignInCommand>,
}

impl Drop for SignInChannels {
    /// Dropping the ECS end is how the app says "stop", exactly as it is for the
    /// session thread: the explicit command is noticed on the listener's next poll
    /// and the channel closing says the same thing to a thread that looks between
    /// the two.
    fn drop(&mut self) {
        let _ = self.commands.send(SignInCommand::Cancel);
    }
}

/// Signs this client in, and keeps the ticket that proves it did.
///
/// **Built only when an account service is configured**, which is deliberate and is
/// the conservative half of this feature: with no `--account-service` there is no
/// login screen, no sign-in state and no behaviour change at all. An account
/// service is something an operator runs and this client cannot invent one, so the
/// shape mirrors `newSignIn` on the other side — the feature says what is missing
/// rather than being silently absent.
pub struct SignInPlugin {
    service: signin::AccountService,
    world: Option<String>,
    ticket_path: Option<PathBuf>,
    browser: signin::Browser,
}

impl SignInPlugin {
    /// Signs in against `service`, keeping the ticket in the file this client
    /// derives for it.
    pub fn new(service: AccountService) -> Self {
        Self {
            service,
            world: None,
            ticket_path: None,
            browser: signin::Browser::System,
        }
    }

    /// Asks for a ticket scoped to `world` rather than an account ticket that names
    /// none, and keeps it in a file of that world's own.
    ///
    /// **This is `--world`, and the reason it is a flag rather than something derived is
    /// worth having in one place.** A ticket names one world; on the list path the row a
    /// player clicked carries that name, and on the `--server` path there is no row. The
    /// address cannot supply it either: what a world is called is the game server's
    /// `-world-name`, which nothing states before the handshake — and a value taken from
    /// the far end would let an address in no list choose which world's ticket it is
    /// handed, which is precisely the choice that has to stay with the developer who
    /// typed the address. So the same command line names both, and neither is inferred
    /// from the other.
    pub fn for_world(mut self, world: impl Into<String>) -> Self {
        self.world = Some(world.into());
        self
    }

    /// Keeps the ticket in `path` rather than in the derived file.
    ///
    /// Test-only today. It is the same seam `--identity` is for the identity file,
    /// and it exists here so the cache can be driven without a home directory.
    #[cfg(test)]
    fn with_ticket_path(mut self, path: PathBuf) -> Self {
        self.ticket_path = Some(path);
        self
    }

    /// Records the authorize URL instead of opening a browser at it. See
    /// [`signin::Browser`] for why the seam cannot be reached from a shipped
    /// client.
    #[cfg(test)]
    fn with_captured_browser(mut self, browser: Sender<String>) -> Self {
        self.browser = signin::Browser::Captured(browser);
        self
    }
}

impl Plugin for SignInPlugin {
    fn build(&self, app: &mut App) {
        // The cache is read here rather than in a system, and `Plugin::build` is
        // the right place for it: it is one bounded read of at most a hundred
        // bytes, it happens once, before a schedule exists — and doing it in a
        // Startup system instead would mean either blocking a frame on a file or
        // spending a frame with the login screen up before anyone knows whether it
        // should be. The rule this respects is "a Bevy *system* never blocks".
        // One file per scope: an account ticket where the list path expects one, and a
        // world ticket in a file of that world's own. Reusing one file for both would
        // put a live credential for the wrong thing behind a screen that says "signed
        // in" and offers no control — see `tickets::world_ticket_path`.
        let ticket_path = self.ticket_path.clone().or_else(|| {
            // The same fallback the session thread uses, and for the same reason: in a
            // test build it names nowhere, so a plugin built without `with_ticket_path`
            // cannot reach the developer's data directory. See
            // `session::default_environment`.
            let env = session::default_environment();
            match self.world.as_deref() {
                Some(world) => tickets::world_ticket_path(self.service.authority(), world, &env),
                None => tickets::default_ticket_path(self.service.authority(), &env),
            }
        });

        let (state, complaint) = match ticket_path.as_deref() {
            Some(path) => {
                let (cached, complaint) = tickets::read(path);
                let state = match cached {
                    Some(cached) if cached.is_live(tickets::now_unix()) => SignInState::SignedIn,
                    Some(_) => SignInState::SignedOut {
                        reason: Some(TICKET_EXPIRED.to_owned()),
                    },
                    None => SignInState::SignedOut { reason: None },
                };
                (state, complaint)
            }
            None => (
                SignInState::SignedOut { reason: None },
                Some(format!(
                    "no file could be named to keep a sign-in for {} in: every launch will need \
                     the browser again.",
                    self.service
                )),
            ),
        };
        if let Some(complaint) = complaint {
            warn!("{complaint}");
        }

        app.insert_resource(state)
            .insert_resource(SignInSettings {
                service: self.service.clone(),
                world: self.world.clone(),
                ticket_path,
                browser: self.browser.clone(),
            })
            .add_message::<SignInRequest>()
            .add_systems(Update, (start_sign_in, drain_sign_in_events).chain());
    }
}

/// Starts one attempt when the login screen asks and nothing is already running.
fn start_sign_in(
    mut requests: MessageReader<SignInRequest>,
    mut state: ResMut<SignInState>,
    settings: Res<SignInSettings>,
    mut commands: Commands,
) {
    // Consume the whole frame's batch: several presses are one sign-in, not one
    // replayed across later frames. The same rule `disconnect_on_request` keeps.
    if requests.read().count() == 0 {
        return;
    }
    // `Waiting` is the guard rather than the resource below, because a `Commands`
    // insert lands at the next sync point and a second press in the meantime would
    // otherwise open a second tab and bind the same port twice.
    if !matches!(*state, SignInState::SignedOut { .. }) {
        return;
    }

    let (event_tx, event_rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel();
    let service = settings.service.clone();
    let world = settings.world.clone();
    let ticket_path = settings.ticket_path.clone();
    let browser = settings.browser.clone();

    match thread::Builder::new()
        .name("voxelheim-signin".to_owned())
        .spawn(move || {
            signin::run(
                service,
                world,
                ticket_path,
                // A shipped client has exactly one way to get the listener: bind
                // the port the account service registered. See `signin::Loopback`.
                signin::Loopback::Bind,
                browser,
                event_tx,
                command_rx,
            );
        }) {
        // Detached, as the session thread is: the app must never wait on a socket
        // to shut down, and dropping the ECS end of the channels is what stops it.
        Ok(_detached) => {
            *state = SignInState::Waiting;
            commands.insert_resource(SignInLink(Mutex::new(SignInChannels {
                events: event_rx,
                commands: command_tx,
            })));
        }
        // Not a panic: a client that cannot start a thread can still say so and
        // leave the control live.
        Err(err) => {
            error!("the sign-in thread would not start: {err}");
            *state = SignInState::SignedOut {
                reason: Some(format!("this client could not start a sign-in: {err}")),
            };
        }
    }
}

/// Publishes whatever the sign-in thread has said, without ever waiting for it.
fn drain_sign_in_events(
    link: Option<ResMut<SignInLink>>,
    mut state: ResMut<SignInState>,
    mut commands: Commands,
) {
    let Some(mut link) = link else {
        return;
    };
    let channels = match link.0.get_mut() {
        Ok(channels) => channels,
        Err(poisoned) => poisoned.into_inner(),
    };

    loop {
        match channels.events.try_recv() {
            Ok(SignInEvent::Warning(text)) => warn!("{text}"),
            Ok(SignInEvent::Completed) => {
                *state = SignInState::SignedIn;
                commands.remove_resource::<SignInLink>();
                return;
            }
            Ok(SignInEvent::Refused(reason)) => {
                *state = SignInState::SignedOut {
                    reason: Some(reason),
                };
                commands.remove_resource::<SignInLink>();
                return;
            }
            Err(TryRecvError::Empty) => return,
            // The thread ended without saying how, which `signin::run` has no path
            // to do — it always sends one terminal event. Handled anyway, because
            // a login screen stuck on "waiting" for ever is the one outcome a
            // player cannot act on.
            Err(TryRecvError::Disconnected) => {
                if *state == SignInState::Waiting {
                    *state = SignInState::SignedOut {
                        reason: Some("the sign-in stopped without saying why.".to_owned()),
                    };
                }
                commands.remove_resource::<SignInLink>();
                return;
            }
        }
    }
}

/// The servers this client may join, as the list screen reads them.
///
/// **There is no fourth variant for "empty", and that is the design.** An empty
/// [`Self::Ready`] is a true statement — no server has registered with the account
/// service — and it is a different thing from [`Self::Unavailable`], which is "nobody
/// could be asked". Collapsing the two would put an empty list in front of a player
/// whose network is down, and an empty list reads as *no servers exist*. The screen
/// renders the second as a line and a retry, never as a list.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub enum ServerList {
    /// A read is in flight, or is about to be started.
    Loading,
    /// What the account service answered, in its order.
    Ready(Vec<ListedServer>),
    /// The list could not be read. `reason` is the line a player reads, and the
    /// screen offers a retry beside it.
    Unavailable(String),
}

/// The list screen asking for the list again.
///
/// A message rather than a direct call, for the reason [`SignInRequest`] is one: `ui`
/// may ask, and only the network boundary may open a socket.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshServerList;

/// The ECS end of a running list read.
///
/// Present exactly while there is a thread reading, which is what makes "is one already
/// running" a question about a resource rather than a flag somebody has to remember to
/// clear — the same shape [`SignInLink`] has. One-way: the read is a single bounded
/// request, so there is no command to send it, and dropping this end simply leaves a
/// thread that finishes and finds nobody listening.
#[derive(Resource)]
struct ServerListLink(Mutex<Receiver<ServerListEvent>>);

/// Reads the server list, and keeps it current enough to click.
///
/// **Built only when an account service is configured**, beside [`SignInPlugin`] and
/// for the same reason: an account service is something an operator runs, and with none
/// there is no list, no login screen and no behaviour change at all. It reads
/// [`SignInSettings`], which that plugin inserts, rather than keeping a second idea of
/// where this client signs in.
pub struct ServerListPlugin;

impl Plugin for ServerListPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ServerList::Loading)
            .add_message::<RefreshServerList>()
            .add_systems(Update, (read_server_list, drain_server_list_events).chain());
    }
}

/// Starts a read when there is a live ticket and nothing to show, or when asked.
fn read_server_list(
    mut requests: MessageReader<RefreshServerList>,
    settings: Option<Res<SignInSettings>>,
    sign_in: Option<Res<SignInState>>,
    mut list: ResMut<ServerList>,
    link: Option<Res<ServerListLink>>,
    mut commands: Commands,
) {
    // Consume the whole frame's batch: several presses of the retry are one read.
    let asked = requests.read().count() > 0;

    // A read is already in flight. The messages are consumed above regardless, so a
    // press during a read is absorbed rather than queued into a second one.
    if link.is_some() {
        return;
    }
    // Nothing to read the list with. The login screen is up and owns the answer.
    // `Option` because a headless test may build this plugin on its own; in the app
    // `SignInPlugin` is what inserts the state, and the two are added together.
    if sign_in.as_deref() != Some(&SignInState::SignedIn) {
        return;
    }
    if asked {
        // Back to `Loading` first, so the screen stops showing the failure it is
        // retrying past while the retry is in flight.
        *list = ServerList::Loading;
    } else if !matches!(*list, ServerList::Loading) {
        return;
    }

    // Absent only in a test that built this plugin without the sign-in one. A read
    // needs somewhere to read from, so there is nothing to do and nothing to claim.
    let Some(settings) = settings else {
        return;
    };

    let (event_tx, event_rx) = mpsc::channel();
    let service = settings.service.clone();
    let ticket_path = settings.ticket_path.clone();

    match thread::Builder::new()
        .name("voxelheim-servers".to_owned())
        .spawn(move || servers::run(service, ticket_path, &event_tx))
    {
        // Detached, as the other two threads are: the app must never wait on a socket
        // to shut down, and dropping the ECS end of the channel is what stops it.
        Ok(_detached) => commands.insert_resource(ServerListLink(Mutex::new(event_rx))),
        // Not a panic, and not an empty list either: a client that cannot start a
        // thread says so and leaves the retry live.
        Err(err) => {
            error!("the server list thread would not start: {err}");
            *list = ServerList::Unavailable(format!(
                "this client could not start reading the server list: {err}"
            ));
        }
    }
}

/// Publishes whatever the list thread said, without ever waiting for it.
fn drain_server_list_events(
    link: Option<ResMut<ServerListLink>>,
    mut list: ResMut<ServerList>,
    sign_in: Option<ResMut<SignInState>>,
    mut commands: Commands,
) {
    let Some(mut link) = link else {
        return;
    };
    let events = match link.0.get_mut() {
        Ok(events) => events,
        Err(poisoned) => poisoned.into_inner(),
    };

    match events.try_recv() {
        Ok(ServerListEvent::Ready(servers)) => {
            info!("the server list holds {} servers", servers.len());
            *list = ServerList::Ready(servers);
        }
        Ok(ServerListEvent::Unavailable(reason)) => {
            warn!("the server list could not be read: {reason}");
            *list = ServerList::Unavailable(reason);
        }
        // The credential, not the network. Answered by sending the player back to the
        // login screen rather than by a retry that would fail the same way — and the
        // list returns to `Loading`, so signing in again reads it without a press.
        Ok(ServerListEvent::SignedOut(reason)) => {
            if let Some(mut sign_in) = sign_in {
                *sign_in = SignInState::SignedOut {
                    reason: Some(reason),
                };
            }
            *list = ServerList::Loading;
        }
        Err(TryRecvError::Empty) => return,
        // The thread ended without saying how, which `servers::run` has no path to do:
        // it always sends one event. Handled anyway, because a list screen stuck on
        // "loading" for ever is the one outcome a player cannot act on.
        Err(TryRecvError::Disconnected) => {
            if matches!(*list, ServerList::Loading) {
                *list = ServerList::Unavailable(
                    "reading the server list stopped without saying why.".to_owned(),
                );
            }
        }
    }
    // Reached on every terminal answer: one read, one thread, and the resource going
    // away is what lets the next retry start one.
    commands.remove_resource::<ServerListLink>();
}

#[cfg(test)]
mod tests {

    use std::io::{Read, Write};

    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::codec::server_side::{
        AppearanceWire, CharacterSummaryWire, DEFAULT_TOKEN, EntityStateWire, WelcomeWire,
        encode_chunk_data, encode_chunk_unload, encode_entity_snapshot, encode_inventory_state,
        encode_leave_cancel_result, encode_leave_started, encode_mine_progress,
        encode_resident_appearance, encode_server_character_list, encode_server_reject,
        encode_server_welcome,
    };

    use super::codec::{PLAYER_TOKEN_LEN, SESSION_TICKET_LEN, SessionTicket};
    use super::frame::FRAME_HEADER_SIZE;
    use super::session::Scratch;
    use super::*;
    use crate::wire::voxelheim::net as fb;

    /// The list [`ConnectionState::every`] hands the sweeps holds every variant.
    ///
    /// **A flag per variant, not a count**, and the difference is the whole test: eight
    /// values with `Rejected` missing and `Idle` written twice counts to eight and covers
    /// seven states. A `match` over the values proves every variant has an *arm*, never that
    /// every variant is *present*.
    ///
    /// What the two together buy is a chain of forced edits that ends in the right place.
    /// A ninth variant stops the match compiling; the arm the author writes indexes past
    /// the array; growing the array makes `all` fail; and the only thing that satisfies it
    /// is adding the state to `every`. Nothing here can be quietened by editing a number.
    #[test]
    fn the_list_holds_every_state() {
        let mut seen = [false; 8];
        for state in ConnectionState::every() {
            match state {
                ConnectionState::Idle => seen[0] = true,
                ConnectionState::Connecting => seen[1] = true,
                ConnectionState::Handshaking => seen[2] = true,
                ConnectionState::Choosing => seen[3] = true,
                ConnectionState::Connected => seen[4] = true,
                ConnectionState::Leaving { .. } => seen[5] = true,
                ConnectionState::Rejected { .. } => seen[6] = true,
                ConnectionState::Disconnected => seen[7] = true,
            }
        }
        assert!(
            seen.iter().all(|seen| *seen),
            "`every` is missing a state: {seen:?}"
        );
    }

    /// How long a test will pump the app waiting for a state. Generous because it
    /// covers a loopback round trip on a loaded CI runner, and irrelevant to
    /// runtime because every assertion is reached long before it.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// What the stub does once it has read the client's `ClientHello`.
    enum Reply {
        /// Answer with these frames, then hold the connection open until the
        /// client closes it. Holding matters: closing immediately would race a
        /// `Connected` assertion against the `Disconnected` that follows.
        ///
        /// **The answer to the hello, which is where a refusal belongs**: a ticket this
        /// server will not admit is refused here, before any character is mentioned.
        Frames(Vec<Vec<u8>>),
        /// Answer the hello with a character list, wait for the client's choice, and
        /// only then send these frames.
        ///
        /// **The ordinary path from V7 on**, and it is a variant of its own rather than
        /// the default because the difference is what each test is about: a welcome
        /// answers a *choice*, and a stub that sent one straight after the hello would be
        /// testing a server this client refuses.
        AfterAChoice(Vec<Vec<u8>>),
        /// Establish a session, then wait for LeaveRequest, acknowledge it and close.
        /// Only this authoritative reply turns a request into a completed leave.
        AfterAChoiceThenLeave {
            frames: Vec<Vec<u8>>,
            remaining_ms: u32,
        },
        /// Establish, acknowledge a leave, then answer the cancellation while retaining
        /// the same socket.
        AfterAChoiceThenCancel {
            frames: Vec<Vec<u8>>,
            remaining_ms: u32,
        },
        /// Answer the hello with a character list, then close without waiting for a
        /// choice — exactly what `-character-timeout` expiring does on the server.
        ListThenClose,
        /// Close without answering.
        Close,
        /// Hold the connection open and say nothing.
        Hold,
    }

    /// The list a stub answers a hello with: one character, and room for more.
    ///
    /// Its contents matter to exactly one test — the one that asserts what reaches the
    /// ECS — and every other test needs only that *something* answerable arrived.
    fn one_character() -> Vec<u8> {
        encode_server_character_list(
            Some(&[CharacterSummaryWire {
                character_id: 900,
                name: Some("Eivor".to_owned()),
                appearance: Some(AppearanceWire::default()),
            }]),
            3,
        )
    }

    /// A one-connection stand-in for `voxelheimd`, speaking the same framing.
    ///
    /// It exists so the plugin can be driven over a real socket without a real
    /// server: the codec is already unit-tested against the contract, and what is
    /// left to prove is that the thread, the channels and the ECS agree.
    ///
    /// It returns **every frame the client sent**, in order, which is the only way to check
    /// the outbound half of the boundary: that a Bevy system's frame actually reaches a
    /// socket, framed the way the server reads it.
    fn spawn_stub(reply: Reply) -> (String, JoinHandle<Vec<Vec<u8>>>) {
        spawn_stub_serving(reply, 1)
    }

    /// The same, answering `connections` clients one after another at one address.
    ///
    /// **A second connection buys exactly one thing, and it is what this client
    /// *remembered* from the first.** The character played on a server is written down per
    /// address, so the two sessions have to reach the same one — which a stub that served
    /// a single connection could not offer.
    fn spawn_stub_serving(reply: Reply, connections: usize) -> (String, JoinHandle<Vec<Vec<u8>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener
            .local_addr()
            .expect("read the stub's own address")
            .to_string();

        let handle = thread::spawn(move || {
            let mut received = Vec::new();
            for _ in 0..connections {
                let Ok((mut socket, _)) = listener.accept() else {
                    return received;
                };
                socket
                    .set_read_timeout(Some(PATIENCE))
                    .expect("a fresh socket accepts a read timeout");
                if !serve_one(&mut socket, &reply, &mut received) {
                    return received;
                }
            }
            received
        });

        (addr, handle)
    }

    /// The same address answering each connection differently.
    ///
    /// A retryable character refusal needs both halves in one test: the first socket
    /// closes after the reject, and the second answers the fresh hello with the list the
    /// form is rebuilt from. Repeating one reply cannot model that sequence without
    /// either refusing forever or welcoming the retry before a person has typed again.
    fn spawn_stub_sequence(replies: Vec<Reply>) -> (String, JoinHandle<Vec<Vec<u8>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener
            .local_addr()
            .expect("read the stub's own address")
            .to_string();

        let handle = thread::spawn(move || {
            let mut received = Vec::new();
            for reply in replies {
                let Ok((mut socket, _)) = listener.accept() else {
                    return received;
                };
                socket
                    .set_read_timeout(Some(PATIENCE))
                    .expect("a fresh socket accepts a read timeout");
                if !serve_one(&mut socket, &reply, &mut received) {
                    return received;
                }
            }
            received
        });

        (addr, handle)
    }

    /// Answers one connection, and reports whether it ended in a way worth carrying on
    /// from.
    ///
    /// Every frame the client sent is pushed onto `received`, in order, which is the only
    /// way to check the outbound half of the boundary: that a Bevy system's frame actually
    /// reaches a socket, framed the way the server reads it.
    fn serve_one(socket: &mut TcpStream, reply: &Reply, received: &mut Vec<Vec<u8>>) -> bool {
        // The hello always comes first: the server would refuse anything else.
        if let Some(frame) = read_one_frame(socket) {
            received.push(frame);
        }

        let send = |socket: &mut TcpStream, payload: &[u8]| {
            let mut framed = (payload.len() as u32).to_be_bytes().to_vec();
            framed.extend_from_slice(payload);
            socket.write_all(&framed).and_then(|()| socket.flush())
        };

        match reply {
            Reply::ListThenClose => {
                let _ = send(socket, &one_character());
                thread::sleep(Duration::from_millis(400));
                return true;
            }
            Reply::Close => return true,
            Reply::Hold => {}
            Reply::Frames(frames) => {
                for payload in frames {
                    if send(socket, payload).is_err() {
                        return false;
                    }
                }
            }
            Reply::AfterAChoice(frames) => {
                // The list, then the choice the client makes, then the answer to it.
                // Reading the choice before answering is what makes this a stand-in for
                // the server rather than a stream of frames: a welcome that overtook the
                // selection would be refused, correctly.
                if send(socket, &one_character()).is_err() {
                    return false;
                }
                match read_one_frame(socket) {
                    Some(choice) => received.push(choice),
                    None => return false,
                }
                for payload in frames {
                    if send(socket, payload).is_err() {
                        return false;
                    }
                }
            }
            Reply::AfterAChoiceThenLeave {
                frames,
                remaining_ms,
            } => {
                if send(socket, &one_character()).is_err() {
                    return false;
                }
                match read_one_frame(socket) {
                    Some(choice) => received.push(choice),
                    None => return false,
                }
                for payload in frames {
                    if send(socket, payload).is_err() {
                        return false;
                    }
                }
                while let Some(frame) = read_one_frame(socket) {
                    let is_leave = fb::root_as_envelope(&frame)
                        .is_ok_and(|envelope| envelope.payload_type() == fb::Payload::LeaveRequest);
                    received.push(frame);
                    if is_leave {
                        return send(socket, &encode_leave_started(*remaining_ms)).is_ok();
                    }
                }
                return false;
            }
            Reply::AfterAChoiceThenCancel {
                frames,
                remaining_ms,
            } => {
                if send(socket, &one_character()).is_err() {
                    return false;
                }
                match read_one_frame(socket) {
                    Some(choice) => received.push(choice),
                    None => return false,
                }
                for payload in frames {
                    if send(socket, payload).is_err() {
                        return false;
                    }
                }
                let mut leave_seen = false;
                while let Some(frame) = read_one_frame(socket) {
                    let kind = fb::root_as_envelope(&frame)
                        .ok()
                        .map(|envelope| envelope.payload_type());
                    received.push(frame);
                    match kind {
                        Some(fb::Payload::LeaveRequest) if !leave_seen => {
                            leave_seen = true;
                            if send(socket, &encode_leave_started(*remaining_ms)).is_err() {
                                return false;
                            }
                        }
                        Some(fb::Payload::LeaveCancelRequest) if leave_seen => {
                            if send(socket, &encode_leave_cancel_result(true, 0)).is_err() {
                                return false;
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Read until the client hangs up, which is how the socket stays open for exactly
        // as long as the client wants it — and how everything sent after the handshake is
        // recorded.
        while let Some(frame) = read_one_frame(socket) {
            received.push(frame);
        }
        true
    }

    /// Reads exactly one length-prefixed frame, the way the server's `ReadFrame`
    /// does.
    fn read_one_frame(socket: &mut TcpStream) -> Option<Vec<u8>> {
        let mut header = [0u8; FRAME_HEADER_SIZE];
        socket.read_exact(&mut header).ok()?;
        let mut payload = vec![0u8; u32::from_be_bytes(header) as usize];
        socket.read_exact(&mut payload).ok()?;
        Some(payload)
    }

    /// The character screen's stand-in: play the first character on offer, or make one.
    ///
    /// Every test in this module is about the boundary rather than about the screen —
    /// `ui/character.rs` is where choosing is tested — but a session that nobody answers
    /// for waits in `Choosing` for ever, which is exactly what the server does. So the
    /// helpers below register this, and the one test that is about the *phase* drives the
    /// message itself.
    fn answer_the_character_phase(
        choice: Option<Res<CharacterChoice>>,
        mut choices: MessageWriter<ChooseCharacter>,
    ) {
        let Some(choice) = choice.filter(|choice| !choice.answered()) else {
            return;
        };
        match choice.characters().first() {
            Some(character) => {
                choices.write(ChooseCharacter::Play(character.character_id));
            }
            None => {
                choices.write(ChooseCharacter::Create {
                    name: "Eivor".to_owned(),
                    appearance: codec::PLACEHOLDER_APPEARANCE,
                });
            }
        }
    }

    /// Builds a headless app: no window, no renderer, no display needed.
    ///
    /// The identity goes to a scratch file the caller holds, never to the real
    /// `$XDG_DATA_HOME`/`$HOME`. Every welcome these tests deliver reaches
    /// `Established`, and that is exactly where `IdentityFile::store` writes — so
    /// leaving the derivation to the net thread puts one file per ephemeral port
    /// in the developer's own data directory, hundreds of them across a few runs,
    /// and a later run reading one back would make a test depend on a previous
    /// one. The returned [`Scratch`] must outlive the test body; dropping it
    /// removes the directory.
    fn headless(addr: &str) -> (App, Scratch) {
        let scratch = Scratch::new("net-headless");
        let identity = scratch.join("identity");
        (headless_with_identity(addr, &identity), scratch)
    }

    /// The same, keeping its identity in a file the test chose.
    fn headless_with_identity(addr: &str, identity: &std::path::Path) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(
                NetPlugin::as_if_listed(addr)
                    .over_plaintext()
                    .with_identity_path(Some(identity.to_path_buf())),
            )
            .add_systems(Update, answer_the_character_phase.after(DrainNetwork));
        app
    }

    /// The token a hello carried, read off the frame the stub recorded.
    fn presented_token(hello: &[u8]) -> Option<Vec<u8>> {
        let envelope = fb::root_as_envelope(hello).expect("the client encodes valid frames");
        envelope
            .payload_as_client_hello()
            .expect("the first frame is a ClientHello")
            .player_token()
            .map(|token| token.bytes().to_vec())
    }

    /// The ticket a hello carried, read off the frame the stub recorded.
    fn presented_ticket(hello: &[u8]) -> Option<Vec<u8>> {
        let envelope = fb::root_as_envelope(hello).expect("the client encodes valid frames");
        envelope
            .payload_as_client_hello()
            .expect("the first frame is a ClientHello")
            .session_ticket()
            .map(|ticket| ticket.bytes().to_vec())
    }

    /// The settings `SignInPlugin` would have inserted for a development launch that
    /// named `world` and keeps its ticket in `ticket_path`.
    ///
    /// Built here rather than by building that plugin, because `SignInPlugin::build`
    /// reads the real data directory to decide where a ticket lives and these tests are
    /// about what happens *after* one exists.
    fn sign_in_settings(world: &str, ticket_path: &std::path::Path) -> SignInSettings {
        SignInSettings {
            service: AccountService::plaintext("http://127.0.0.1:7780").expect("a service URL"),
            world: Some(world.to_owned()),
            ticket_path: Some(ticket_path.to_path_buf()),
            browser: signin::Browser::System,
        }
    }

    /// A cached ticket of `byte`s that will not expire during a test run.
    fn live_ticket(byte: u8) -> tickets::CachedTicket {
        tickets::CachedTicket::new(
            SessionTicket::from_bytes([byte; SESSION_TICKET_LEN]),
            tickets::now_unix() + 3_600,
        )
    }

    /// The name a hello announced.
    fn announced_name(hello: &[u8]) -> String {
        let envelope = fb::root_as_envelope(hello).expect("the client encodes valid frames");
        envelope
            .payload_as_client_hello()
            .expect("the first frame is a ClientHello")
            .player_name()
            .unwrap_or_default()
            .to_owned()
    }

    /// Runs frames until `done` holds, or fails the test at [`PATIENCE`].
    fn pump_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            app.update();
            if done(app) {
                return;
            }
            // The net thread needs a moment to make progress, and a spin would
            // starve it on a single-core runner.
            thread::sleep(Duration::from_millis(2));
        }
        panic!(
            "timed out waiting for {what}; state is {:?}",
            app.world().resource::<ConnectionState>()
        );
    }

    fn state(app: &App) -> ConnectionState {
        app.world().resource::<ConnectionState>().clone()
    }

    /// **The measurement #627 turned on, as a test.** A server that closes on
    /// `-character-timeout` writes nothing back — that is deliberate, there is no
    /// message for a reply to be the answer to — so all the client has is `read`
    /// returning zero while the handshake sits in `Choosing`.
    ///
    /// It used to arrive as a *refusal*, because `peer_closed` asked only whether the
    /// handshake was `established()`: the client was left in `Rejected` reading
    /// "closed the connection before answering the handshake", which is untrue of this
    /// case — the server answered it with a character list, which is why there was a
    /// screen to be timed out on. It is an ending, and the screen for an ending is the
    /// one that says a session ended.
    ///
    /// The other half of the same assertion is the character screen coming down:
    /// `character_is_up` is `CharacterChoice` being present and nothing else, so a
    /// resource left behind here would be a character screen over a dead socket.
    #[test]
    fn a_character_timeout_close_ends_the_session_rather_than_refusing_it() {
        let (addr, stub) = spawn_stub(Reply::ListThenClose);
        let scratch = Scratch::new("net-character-timeout");
        let identity = scratch.join("identity");

        // Deliberately without `answer_the_character_phase`: this is the player who
        // chooses nobody, which is the whole of the reproduction.
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(
            NetPlugin::as_if_listed(&addr)
                .over_plaintext()
                .with_identity_path(Some(identity)),
        );

        pump_until(&mut app, "the character screen", |app| {
            app.world().contains_resource::<CharacterChoice>()
        });
        assert_eq!(state(&app), ConnectionState::Choosing);

        pump_until(&mut app, "the timeout to close the session", |app| {
            state(app) == ConnectionState::Disconnected
        });
        assert!(
            !app.world().contains_resource::<CharacterChoice>(),
            "the character screen stayed up over a session that had ended"
        );
        // And there is still a server to offer a way back to, which is what the screen
        // reads to decide whether to draw one.
        assert_eq!(app.world().resource::<ServerAddress>().0, addr);

        drop(app);
        let _ = stub.join();
    }

    /// A client holding a route, a terminal state and no live thread — everything a
    /// press has to act on, and nothing that would dial on its own.
    fn ready_to_reconnect(state: ConnectionState) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(state)
            .insert_resource(SessionSettings {
                player_name: DEFAULT_PLAYER_NAME.to_owned(),
                identity_path: None,
                data_home: None,
                transport: session::Transport::Plaintext,
            })
            .add_message::<ConnectRequest>()
            .add_message::<ReconnectRequest>()
            .add_systems(Update, reconnect_on_request);
        app
    }

    fn dials_asked_for(app: &App) -> Vec<ConnectRequest> {
        let messages = app.world().resource::<Messages<ConnectRequest>>();
        let mut cursor = messages.get_cursor();
        cursor.read(messages).cloned().collect()
    }

    /// **A press dials the server the session was on, by the route it was opened by.**
    /// The row's name rather than an address, so the address and the fingerprint to
    /// expect at it come back out of that row together — which is what `RejoinBy` is
    /// for, and why the message a press writes carries nothing at all.
    #[test]
    fn a_reconnect_asks_for_the_row_the_last_session_was_opened_by() {
        let mut app = ready_to_reconnect(ConnectionState::Disconnected);
        app.insert_resource(RejoinBy::Row("midgard".to_owned()));

        app.world_mut().write_message(ReconnectRequest);
        app.update();

        assert_eq!(
            dials_asked_for(&app),
            vec![ConnectRequest {
                name: "midgard".to_owned()
            }]
        );
        // And it is one dial rather than one replayed: the batch is consumed, and no
        // flag is left behind for a later frame to act on.
        for _ in 0..5 {
            app.update();
        }
        assert_eq!(dials_asked_for(&app).len(), 1, "a press dialled twice");
        assert!(
            !app.world().contains_resource::<Rejoining>(),
            "a press armed the character screen's internal retry"
        );
    }

    /// **Nothing dials without a press**, which is the constraint the whole issue turns
    /// on: a client that redialled on its own would be hammering a server that had just
    /// closed it. Everything a reconnection needs is in place here except the press.
    #[test]
    fn nothing_dials_until_somebody_presses_something() {
        let mut app = ready_to_reconnect(ConnectionState::Disconnected);
        app.insert_resource(RejoinBy::Row("midgard".to_owned()));

        for _ in 0..16 {
            app.update();
        }

        assert!(
            dials_asked_for(&app).is_empty(),
            "the client dialled itself"
        );
        assert_eq!(state(&app), ConnectionState::Disconnected);
    }

    /// A press with nothing to go back to asks for nothing rather than guessing at a
    /// server. The screen does not draw the control in `Idle`; this is the boundary
    /// answering for itself, because a message is a thing anything can write.
    #[test]
    fn a_reconnect_with_no_route_dials_nothing() {
        let mut app = ready_to_reconnect(ConnectionState::Idle);

        app.world_mut().write_message(ReconnectRequest);
        app.update();

        assert!(dials_asked_for(&app).is_empty());
        assert_eq!(state(&app), ConnectionState::Idle);
    }

    /// And a press that arrives while a session is being opened is ignored: `Idle` is
    /// not a state a reconnection means anything in, and the route is only followed
    /// from a session that is over.
    #[test]
    fn a_reconnect_from_a_state_with_no_ended_session_dials_nothing() {
        for state in [
            ConnectionState::Idle,
            ConnectionState::Connecting,
            ConnectionState::Handshaking,
            ConnectionState::Choosing,
            ConnectionState::Connected,
            ConnectionState::Leaving {
                seconds_remaining: Some(3),
            },
        ] {
            let mut app = ready_to_reconnect(state.clone());
            app.insert_resource(RejoinBy::Row("midgard".to_owned()));

            app.world_mut().write_message(ReconnectRequest);
            app.update();

            assert!(dials_asked_for(&app).is_empty(), "{state:?} dialled again");
        }
    }

    /// **The launch #154 exists for, end to end inside this module.** Signed out,
    /// nothing is dialled — a hello with no ticket would be refused, so connecting
    /// first would put a refusal on screen underneath the sign-in that was about to fix
    /// it. Signed in, the address the developer typed is dialled and the cached ticket
    /// reaches the wire.
    #[test]
    fn a_development_launch_with_an_account_service_dials_only_once_it_has_a_ticket() {
        let (addr, stub) = spawn_stub(Reply::Hold);
        let scratch = Scratch::new("net-dev-ticket");
        let ticket_path = scratch.join("world-ticket");

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(NetPlugin::developing_against_signed_in(&addr).over_plaintext())
            .insert_resource(SignInState::SignedOut { reason: None })
            .insert_resource(sign_in_settings("midgard", &ticket_path));

        for _ in 0..8 {
            app.update();
        }
        assert_eq!(state(&app), ConnectionState::Idle);
        assert!(
            !app.world().contains_resource::<NetLink>(),
            "a signed-out launch dialled anyway"
        );
        assert!(
            !app.world().contains_resource::<ServerAddress>(),
            "a signed-out launch published an address"
        );

        // What a finished sign-in leaves behind: a ticket in the file this launch
        // named. The state change is the ECS half of the same event.
        tickets::write(&ticket_path, live_ticket(0x7A)).expect("the scratch file is writable");
        *app.world_mut().resource_mut::<SignInState>() = SignInState::SignedIn;

        pump_until(&mut app, "the session to open", |app| {
            app.world().contains_resource::<NetLink>()
        });
        assert_eq!(app.world().resource::<ServerAddress>().0, addr);

        drop(app);
        let received = stub.join().expect("the stub thread");
        let hello = received.first().expect("a hello reached the socket");
        assert_eq!(
            presented_ticket(hello),
            Some(vec![0x7A; SESSION_TICKET_LEN]),
            "the ticket the sign-in cached did not reach the wire"
        );
    }

    /// A launch that says it is signed in and has nothing to present is refused with a
    /// sentence naming the remedy, **and the refusal happens before anything is
    /// dialled** — which is what the unreachable address here proves: a session that
    /// had opened a socket first would have failed with "cannot reach" instead.
    #[test]
    fn a_ticket_that_is_not_there_is_refused_before_a_socket_is_opened() {
        let scratch = Scratch::new("net-no-ticket");
        let ticket_path = scratch.join("world-ticket");

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            // Port 9 discards, so reaching it at all would be the failure under test.
            .add_plugins(NetPlugin::developing_against_signed_in("127.0.0.1:9").over_plaintext())
            .insert_resource(SignInState::SignedIn)
            .insert_resource(sign_in_settings("midgard", &ticket_path));

        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });
        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(reason.contains("sign in again"), "{reason}");
        assert!(!reason.contains("cannot reach"), "{reason}");
    }

    /// An expired cache is the same answer as an empty one. The ticket carries its own
    /// signed expiry and the server is the authority on it; what this buys is a
    /// sentence a player can act on instead of a handshake that ends in the server's
    /// more general refusal.
    #[test]
    fn a_ticket_that_has_run_out_is_not_presented() {
        let scratch = Scratch::new("net-stale-ticket");
        let ticket_path = scratch.join("world-ticket");
        tickets::write(
            &ticket_path,
            tickets::CachedTicket::new(
                SessionTicket::from_bytes([0x11; SESSION_TICKET_LEN]),
                tickets::now_unix() - 1,
            ),
        )
        .expect("the scratch file is writable");

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(NetPlugin::developing_against_signed_in("127.0.0.1:9").over_plaintext())
            .insert_resource(SignInState::SignedIn)
            .insert_resource(sign_in_settings("midgard", &ticket_path));

        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });
        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(reason.contains("sign in again"), "{reason}");
    }

    /// **A refusal is not a retry.** There is no list on this path and therefore no row
    /// to click again, so the one thing a redial would produce is the same refusal on a
    /// loop against a server that has already said no.
    #[test]
    fn a_refused_development_session_is_not_dialled_again() {
        let scratch = Scratch::new("net-no-redial");
        let ticket_path = scratch.join("world-ticket");

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(NetPlugin::developing_against_signed_in("127.0.0.1:9").over_plaintext())
            .insert_resource(SignInState::SignedIn)
            .insert_resource(sign_in_settings("midgard", &ticket_path));

        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        // A ticket arriving afterwards does not restart it either: the state is no
        // longer `Idle`, which is the whole of the rule.
        tickets::write(&ticket_path, live_ticket(0x22)).expect("the scratch file is writable");
        for _ in 0..8 {
            app.update();
        }
        assert!(
            matches!(state(&app), ConnectionState::Rejected { .. }),
            "a refused development session dialled again"
        );
    }

    /// The other half of the same rule, and the one that keeps #150's guarantee true:
    /// a session opened from a list row presents the identity file and **no ticket**,
    /// because the ticket a sign-in caches on that path names no world and a game
    /// server refuses one of those. Joining from the list is #107.
    #[test]
    fn a_listed_session_presents_no_ticket() {
        let (addr, stub) = spawn_stub(Reply::Hold);
        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Connecting", |app| {
            !matches!(state(app), ConnectionState::Idle)
        });
        drop(app);

        let received = stub.join().expect("the stub thread");
        let hello = received.first().expect("a hello reached the socket");
        assert_eq!(presented_ticket(hello), None);
    }

    #[test]
    fn the_plugin_registers_its_resources_without_a_window() {
        let (addr, _stub) = spawn_stub(Reply::Hold);
        let (app, _scratch) = headless(&addr);
        let world = app.world();

        assert!(world.contains_resource::<ConnectionState>());
        assert!(world.contains_resource::<ServerAddress>());
        assert!(world.contains_resource::<NetLink>());
        assert_eq!(world.resource::<ServerAddress>().0, addr);
        assert!(
            !world.contains_resource::<Session>(),
            "there is no session until a welcome arrives"
        );
        assert_eq!(
            *world.resource::<ConnectionState>(),
            ConnectionState::Connecting
        );
    }

    #[test]
    fn a_welcome_reaches_connected_and_publishes_the_session() {
        let welcome = WelcomeWire {
            entity_id: 9,
            spawn: Some([0.5, 80.0, 0.5]),
            world_seed: -12345,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            ..WelcomeWire::default()
        };
        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(&welcome)]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        let session = *app.world().resource::<Session>();
        assert_eq!(session.0.entity_id, 9);
        assert_eq!(session.0.spawn, [0.5, 80.0, 0.5]);
        assert_eq!(session.0.world_seed, -12345);
        assert_eq!(session.0.tick_rate, 20);
        assert_eq!(session.0.chunk_size, 32);
        assert_eq!(session.0.view_distance, 8);

        // The client's half of the exchange, read off a real socket: the server
        // would have refused anything else.
        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let hello = sent
            .first()
            .expect("the client sends a ClientHello before anything else");
        assert_eq!(
            super::codec::decode(hello),
            Ok(super::codec::Message::ClientOnly("ClientHello"))
        );
    }

    #[test]
    fn a_first_connection_presents_nothing_and_stores_what_it_is_given() {
        let scratch = Scratch::new("net-first");
        let identity = scratch.join("identity");
        let welcome = WelcomeWire::default();
        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(&welcome)]));

        let mut app = headless_with_identity(&addr, &identity);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        assert_eq!(
            *app.world().resource::<Identity>(),
            Identity::New,
            "nothing was presented, so this is a new character"
        );

        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let hello = sent.first().expect("the client says hello first");
        assert_eq!(presented_token(hello), None, "there was nothing to present");

        // And the welcome's token is on disk for next time.
        assert_eq!(
            std::fs::read(&identity).expect("the identity file was written"),
            DEFAULT_TOKEN.to_vec()
        );
    }

    /// **A session that kept no token says so rather than guessing**, and this is the
    /// path where the guess would have been wrong: a ticket names an account, the
    /// account decides which character the server restores, and the contract is
    /// explicit that the client is not told which of the two happened. `New` here would
    /// be the client contradicting the server about a fact only the server has.
    #[test]
    fn a_session_that_kept_no_token_reports_neither_new_nor_returning() {
        let scratch = Scratch::new("net-untold");
        let ticket_path = scratch.join("world-ticket");
        tickets::write(&ticket_path, live_ticket(0x33)).expect("the scratch file is writable");
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(NetPlugin::developing_against_signed_in(&addr).over_plaintext())
            .insert_resource(SignInState::SignedIn)
            .insert_resource(sign_in_settings("midgard", &ticket_path))
            .add_systems(Update, answer_the_character_phase.after(DrainNetwork));

        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });
        assert_eq!(*app.world().resource::<Identity>(), Identity::Untold);
    }

    #[test]
    fn a_stored_token_is_presented_and_a_matching_welcome_is_a_returning_session() {
        let scratch = Scratch::new("net-returning");
        let identity = scratch.join("identity");
        std::fs::write(&identity, DEFAULT_TOKEN).expect("a writable scratch directory");

        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let mut app = headless_with_identity(&addr, &identity);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        assert_eq!(
            *app.world().resource::<Identity>(),
            Identity::Returning,
            "the welcome carried the token that was presented"
        );

        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let hello = sent.first().expect("the client says hello first");
        assert_eq!(
            presented_token(hello),
            Some(DEFAULT_TOKEN.to_vec()),
            "the stored token reaches the wire"
        );
    }

    /// **The rule the pin file used to enforce, now enforced by the type.** A session
    /// against a server nothing named a certificate for presents no stored identity —
    /// not because a check refuses it, but because that variant never opens the file.
    ///
    /// Asserted on the wire rather than on a flag: the hello is the only place a token
    /// could cross, and the file beside it is proof the token was there to be sent.
    #[test]
    fn an_unlisted_server_is_never_shown_the_identity_this_client_holds() {
        let scratch = Scratch::new("net-unlisted");
        let identity = scratch.join("identity");
        std::fs::write(&identity, [0x11; PLAYER_TOKEN_LEN]).expect("a writable directory");

        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(
                NetPlugin::developing_against(&addr)
                    .over_plaintext()
                    .with_identity_path(Some(identity.clone())),
            )
            .add_systems(Update, answer_the_character_phase.after(DrainNetwork));
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        // And nothing is kept either, so the *next* launch cannot be a returning
        // session holding a token no verified server ever issued.
        assert_eq!(
            std::fs::read(&identity).expect("the file is still there"),
            vec![0x11; PLAYER_TOKEN_LEN],
            "an unlisted session wrote the welcome's token over the identity file"
        );

        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let hello = sent.first().expect("the client says hello first");
        assert_eq!(
            presented_token(hello),
            None,
            "a stored identity reached a server nothing stated a certificate for"
        );
    }

    /// The control for the test above, and it is what makes it mean anything: the very
    /// same file, the same stub and the same token *is* presented when the expectation
    /// came from a list. Without this, "no token on the wire" could be a client that
    /// never reads an identity file at all.
    #[test]
    fn a_listed_server_is_shown_the_identity_the_unlisted_one_was_not() {
        let scratch = Scratch::new("net-listed-control");
        let identity = scratch.join("identity");
        std::fs::write(&identity, [0x11; PLAYER_TOKEN_LEN]).expect("a writable directory");

        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let mut app = headless_with_identity(&addr, &identity);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let hello = sent.first().expect("the client says hello first");
        assert_eq!(
            presented_token(hello),
            Some(vec![0x11; PLAYER_TOKEN_LEN]),
            "a server the list named was not shown the identity this client holds"
        );
    }

    // -----------------------------------------------------------------------
    // The character phase, end to end across the thread boundary
    // -----------------------------------------------------------------------

    /// **The whole exchange, over a real socket.** A hello is answered with the account's
    /// characters, they reach the ECS as a resource a screen can draw, the choice made
    /// against it reaches the wire as a `SelectCharacterRequest`, and the welcome that
    /// answers it is what makes a session.
    #[test]
    fn a_hello_is_answered_with_characters_and_the_choice_reaches_the_wire() {
        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));
        let (mut app, _scratch) = headless(&addr);

        // The list first, and the state that says the game is waiting for a person.
        pump_until(&mut app, "the character list", |app| {
            app.world().contains_resource::<CharacterChoice>()
        });
        let choice = app.world().resource::<CharacterChoice>().clone();
        assert_eq!(state(&app), ConnectionState::Choosing);
        assert_eq!(choice.characters().len(), 1);
        assert_eq!(choice.characters()[0].character_id, 900);
        assert_eq!(choice.characters()[0].name, "Eivor");
        assert_eq!(choice.max_characters(), 3);
        // Nothing is said about `answered` here: the stand-in for the screen runs after
        // the drain and inside the same frame, so by the time this reads the resource the
        // choice has already gone out. What that flag stops is a *second* press, and
        // `ui/character.rs` is where a press is.

        // And then the answer, which the stand-in for the screen writes.
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });
        assert!(
            !app.world().contains_resource::<CharacterChoice>(),
            "the exchange is over and the screen has nothing left to draw"
        );

        drop(app);
        let received = stub.join().expect("the stub thread");
        assert_eq!(received.len(), 2, "hello, then the choice");
        let envelope = fb::root_as_envelope(&received[1]).expect("the client encodes valid frames");
        let selection = envelope
            .payload_as_select_character_request()
            .expect("the choice is a selection");
        assert_eq!(
            selection.character_id(),
            900,
            "the client echoed back an id the server did not mint"
        );
    }

    /// A welcome that answers no choice ends the connection with a reason a player can
    /// read.
    ///
    /// **The spawn in a welcome belongs to a character**, so one that arrives before the
    /// picking carries a position for somebody nobody chose. `Reply::Frames` is what makes
    /// this a *server* that skipped the phase rather than a client that failed to answer.
    #[test]
    fn a_welcome_that_answers_no_choice_is_refused() {
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));
        let (mut app, _scratch) = headless(&addr);

        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });
        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(reason.contains("chosen"), "{reason}");
        assert!(
            !app.world().contains_resource::<Session>(),
            "a welcome nobody asked for made a session"
        );
    }

    /// Both server-owned name judgements reopen the same creation form over a fresh
    /// connection, carrying the reason that explains what the player should change.
    ///
    /// The server closes after `ServerReject`; three recorded frames prove the retry is
    /// a real redial rather than display state pretending the old socket survived:
    /// hello + creation on the first connection, then a new hello on the second. The
    /// second stub stops at its character list so the client cannot reach `Connected`
    /// and accidentally satisfy this test with a world behind the form.
    #[test]
    fn character_name_refusals_reopen_the_creation_exchange() {
        for (code, name) in [
            (fb::RejectReason::CHARACTER_NAME_TAKEN, "Eivor"),
            (fb::RejectReason::CHARACTER_NAME_REFUSED, "   "),
        ] {
            let detail = "choose another name";
            let (addr, stub) = spawn_stub_sequence(vec![
                Reply::AfterAChoice(vec![encode_server_reject(code, detail)]),
                Reply::Frames(vec![one_character()]),
            ]);
            let scratch = Scratch::new("net-name-retry");
            let mut app = App::new();
            app.add_plugins(MinimalPlugins).add_plugins(
                NetPlugin::as_if_listed(&addr)
                    .over_plaintext()
                    .with_identity_path(Some(scratch.join("identity")))
                    .with_data_home(scratch.join("data")),
            );

            pump_until(&mut app, "the first character list", |app| {
                app.world().contains_resource::<CharacterChoice>()
            });
            app.world_mut().write_message(ChooseCharacter::Create {
                name: name.to_owned(),
                appearance: codec::PLACEHOLDER_APPEARANCE,
            });

            pump_until(&mut app, "the creation form on the new connection", |app| {
                app.world()
                    .get_resource::<CharacterChoice>()
                    .is_some_and(|choice| !choice.answered() && choice.creation_refusal().is_some())
                    && state(app) == ConnectionState::Choosing
            });

            let choice = app.world().resource::<CharacterChoice>();
            let reason = choice
                .creation_refusal()
                .expect("the form carries the server's answer");
            assert_eq!(
                Reject::split_description(reason).0,
                code.variant_name().unwrap()
            );
            assert!(reason.contains(detail), "{reason}");
            assert!(
                !app.world().contains_resource::<Session>(),
                "a retryable creation refusal entered a world"
            );

            drop(app);
            let sent = stub.join().expect("the stub thread must not panic");
            assert_eq!(sent.len(), 3, "hello + creation, then the retry hello");
        }
    }

    /// A creation does not make every `ServerReject` retryable. `BAD_REQUEST` may mean
    /// the frame or appearance was invalid, and typing another name is not its remedy.
    #[test]
    fn bad_request_after_creation_remains_terminal() {
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_reject(
            fb::RejectReason::BAD_REQUEST,
            "that request cannot be accepted",
        )]));
        let scratch = Scratch::new("net-name-retry-negative");
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(
            NetPlugin::as_if_listed(&addr)
                .over_plaintext()
                .with_identity_path(Some(scratch.join("identity")))
                .with_data_home(scratch.join("data")),
        );

        pump_until(&mut app, "the character list", |app| {
            app.world().contains_resource::<CharacterChoice>()
        });
        app.world_mut().write_message(ChooseCharacter::Create {
            name: "Eivor".to_owned(),
            appearance: codec::PLACEHOLDER_APPEARANCE,
        });
        pump_until(&mut app, "the terminal rejection", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        assert!(
            !app.world().contains_resource::<CharacterChoice>(),
            "a non-name rejection returned to the creation form"
        );
        assert!(
            !app.world().contains_resource::<Rejoining>(),
            "a non-name rejection scheduled another connection"
        );
    }

    /// **Leaving a world lands back on its character screen**, over a real socket.
    ///
    /// The whole loop, end to end: a session is established, the player asks to leave, and
    /// the client dials the same address again and stops at the character list. Two
    /// connections from the stub, because the second one is the point.
    ///
    /// It asserts `CharacterChoice` rather than `ConnectionState::Choosing` because the
    /// resource is what the screen is drawn from — the state is how the status line
    /// describes it, and a screen nobody could see would satisfy the wrong one.
    #[test]
    fn leaving_a_world_dials_it_again_and_stops_at_the_character_list() {
        let scratch = Scratch::new("net-rejoin");
        let (addr, _stub) = spawn_stub_sequence(vec![
            Reply::AfterAChoiceThenLeave {
                frames: vec![encode_server_welcome(&WelcomeWire::default())],
                remaining_ms: 25,
            },
            Reply::AfterAChoice(vec![encode_server_welcome(&WelcomeWire::default())]),
        ]);

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(
                NetPlugin::as_if_listed(&addr)
                    .over_plaintext()
                    .with_identity_path(Some(scratch.join("identity")))
                    .with_data_home(scratch.join("data")),
            )
            .add_systems(Update, answer_the_character_phase.after(DrainNetwork));

        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });
        assert!(!app.world().contains_resource::<CharacterChoice>());

        app.world_mut().write_message(DisconnectRequest);

        // The list, not merely a state: it is fetched again over a second connection
        // rather than reused, which is what makes it what the server holds now.
        pump_until(&mut app, "the character list again", |app| {
            app.world().contains_resource::<CharacterChoice>()
        });
        assert_eq!(state(&app), ConnectionState::Choosing);
        assert!(
            !app.world().contains_resource::<Session>(),
            "the rejoin went straight back into the world instead of stopping to ask"
        );

        // And choosing from there enters the world again, through the ordinary handshake.
        pump_until(&mut app, "Connected again", |app| {
            state(app) == ConnectionState::Connected
        });
    }

    #[test]
    fn an_accepted_cancellation_resumes_the_same_socket_end_to_end() {
        let (addr, stub) = spawn_stub(Reply::AfterAChoiceThenCancel {
            frames: vec![encode_server_welcome(&WelcomeWire::default())],
            remaining_ms: 8_000,
        });
        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        app.world_mut().write_message(DisconnectRequest);
        pump_until(&mut app, "the leave acknowledgement", |app| {
            matches!(
                state(app),
                ConnectionState::Leaving {
                    seconds_remaining: Some(_),
                    ..
                }
            )
        });
        app.world_mut().write_message(CancelLeaveRequest);
        pump_until(&mut app, "the accepted cancellation", |app| {
            state(app) == ConnectionState::Connected
        });

        assert!(app.world().contains_resource::<Session>());
        assert!(app.world().contains_resource::<Outbound>());
        assert!(!app.world().contains_resource::<SuspendedOutbound>());
        assert!(!app.world().contains_resource::<Rejoining>());

        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let kinds: Vec<_> = sent
            .iter()
            .filter_map(|frame| {
                fb::root_as_envelope(frame)
                    .ok()
                    .map(|envelope| envelope.payload_type())
            })
            .collect();
        assert!(kinds.contains(&fb::Payload::LeaveRequest));
        assert!(kinds.contains(&fb::Payload::LeaveCancelRequest));
    }

    /// A connection that dropped is reported and stays reported.
    ///
    /// **The line #184 draws.** A dropped connection inserts no `Rejoining`; the only
    /// failure that does is a character-name reject answering a creation, whose complete
    /// remedy is another name on the same form. There is still no reconnect or backoff
    /// policy for a session that ended on its own.
    #[test]
    fn a_session_that_ended_on_its_own_does_not_dial_again() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connected);
        app.insert_resource(RejoinBy::Row("midgard".to_owned()))
            .add_message::<ConnectRequest>()
            .add_systems(Update, rejoin_for_a_character.before(drain_session_events));

        events
            .send(SessionEvent::Ended(Some("the peer went away".to_owned())))
            .expect("the app holds the receiver");
        app.update();
        drop(events);
        for _ in 0..3 {
            app.update();
        }

        assert_eq!(state(&app), ConnectionState::Disconnected);
        assert!(
            !app.world().contains_resource::<Rejoining>(),
            "a dropped connection asked to rejoin"
        );
        let messages = app.world().resource::<Messages<ConnectRequest>>();
        let mut cursor = messages.get_cursor();
        assert_eq!(
            cursor.read(messages).count(),
            0,
            "a dropped connection dialled again"
        );
    }

    /// A game the player was inside, ending for a reason: one notice for the UI to turn
    /// into a chat line, on top of the `warn!` this already reached.
    #[test]
    fn an_established_session_that_ends_with_a_reason_queues_one_ending_notice() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connected);

        events
            .send(SessionEvent::Ended(Some("the peer went away".to_owned())))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            app.world_mut().resource_mut::<SessionEndingInbox>().take(),
            1,
            "an established session ending with a reason must queue exactly one notice"
        );
    }

    /// The ordinary, reasonless close that follows a completed leave is not a failure:
    /// nothing is queued for chat, only for the log.
    #[test]
    fn an_established_session_that_ends_without_a_reason_queues_no_notice() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connected);

        events
            .send(SessionEvent::Ended(None))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            app.world_mut().resource_mut::<SessionEndingInbox>().take(),
            0,
            "an ordinary close must not reach chat"
        );
    }

    /// A close while a character is still being chosen has its own screen already; it must
    /// not also produce a chat line for a game that was never established.
    #[test]
    fn a_session_ending_while_choosing_a_character_queues_no_notice() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Choosing);

        events
            .send(SessionEvent::Ended(Some(
                "closed while a character was being chosen".to_owned(),
            )))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            app.world_mut().resource_mut::<SessionEndingInbox>().take(),
            0,
            "a character-choosing screen must answer for its own ending"
        );
    }

    /// A refusal keeps its reason, and a rejoin that is refused keeps that one too.
    ///
    /// The fourth acceptance criterion, and the half of it that has teeth: returning to a
    /// screen must not swallow why. `Rejoining` is dropped before the dial that consumes
    /// it can fail, so a refused rejoin is a refusal a player can read rather than the
    /// first turn of a loop.
    #[test]
    fn a_refused_rejoin_reports_why_and_stops() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ConnectionState::Disconnected)
            .insert_resource(Rejoining)
            .insert_resource(RejoinBy::Address {
                // Port 0 is not an address a client can dial, so the attempt fails the way
                // an unreachable server does — with a reason, and without a thread.
                addr: "127.0.0.1:0".to_owned(),
                expected: tls::Expectation::Unlisted,
                ticket_path: None,
            })
            .insert_resource(SessionSettings {
                player_name: DEFAULT_PLAYER_NAME.to_owned(),
                identity_path: None,
                data_home: None,
                transport: session::Transport::Plaintext,
            })
            .add_message::<ConnectRequest>()
            .add_systems(Update, rejoin_for_a_character);

        app.update();
        app.update();
        app.update();

        assert!(
            !app.world().contains_resource::<Rejoining>(),
            "the flag survived the dial, so the next frame would try again"
        );
        // `start_session` fails only when the *thread* will not start; an address nothing
        // answers on fails later, over the socket, and arrives as a `SessionEvent` the
        // ordinary way. Either outcome is fine and neither is a loop — which is what the
        // flag above is the assertion for. What must not happen is the state going back to
        // one this client would dial from again on its own.
        assert!(
            matches!(
                state(&app),
                ConnectionState::Connecting | ConnectionState::Rejected { .. }
            ),
            "a failed rejoin left {:?}",
            state(&app)
        );
    }

    /// **The character a session played is the one the next launch starts on.**
    ///
    /// Two sessions at one address: the first plays a character, and the second is offered
    /// the same list and starts on it. Nothing about the preselection is sent — it is a
    /// note this client keeps, matched against the list the server sent — and it is worth
    /// exactly one keypress, which is the whole of what it claims to be.
    ///
    /// A creation is deliberately not remembered, and the wire is why: `ServerWelcome`
    /// names an entity and no character, so a client that has just made one cannot know
    /// the id the server minted for it.
    #[test]
    fn the_character_played_is_the_one_preselected_next_time() {
        let scratch = Scratch::new("net-remembers");
        let (addr, _stub) = spawn_stub_serving(
            Reply::AfterAChoice(vec![encode_server_welcome(&WelcomeWire::default())]),
            2,
        );

        let mut preselections = Vec::new();
        for _ in 0..2 {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .add_plugins(
                    NetPlugin::as_if_listed(&addr)
                        .over_plaintext()
                        .with_identity_path(Some(scratch.join("identity")))
                        .with_data_home(scratch.join("data")),
                )
                .add_systems(Update, answer_the_character_phase.after(DrainNetwork));

            pump_until(&mut app, "the character list", |app| {
                app.world().contains_resource::<CharacterChoice>()
            });
            preselections.push(app.world().resource::<CharacterChoice>().preselect());

            pump_until(&mut app, "Connected", |app| {
                state(app) == ConnectionState::Connected
            });
            drop(app);
        }

        assert_eq!(
            preselections,
            vec![None, Some(900)],
            "the first visit remembers nothing and the second starts on what was played"
        );
    }

    /// A client that takes its servers from a list opens nothing until a row is
    /// clicked. There is no address to name and no socket to have failed.
    #[test]
    fn a_client_waiting_for_the_list_dials_nothing() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(NetPlugin::listening().over_plaintext());
        app.update();

        let world = app.world();
        assert_eq!(*world.resource::<ConnectionState>(), ConnectionState::Idle);
        assert!(
            !world.contains_resource::<NetLink>(),
            "a client with no chosen server started a network thread"
        );
        assert!(
            !world.contains_resource::<ServerAddress>(),
            "a client with no chosen server named an address"
        );
    }

    /// **Clicking a row is what opens a session, and the row is what it is opened
    /// against.** The address the session dials is the row's — the half of "the address
    /// comes from the list on every launch" that this module owns.
    #[test]
    fn a_connect_request_dials_the_server_the_row_named() {
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let scratch = Scratch::new("net-clicked");
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(
                NetPlugin::listening()
                    .over_plaintext()
                    .with_identity_path(Some(scratch.join("identity"))),
            )
            .insert_resource(ServerList::Ready(vec![ListedServer::for_a_test(
                "midgard", &addr, true,
            )]))
            .add_systems(Update, answer_the_character_phase.after(DrainNetwork));
        app.update();

        app.world_mut().write_message(ConnectRequest {
            name: "midgard".to_owned(),
        });
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        assert_eq!(app.world().resource::<ServerAddress>().0, addr);
    }

    /// A name that is not in the list this client holds is refused rather than guessed
    /// at. There is nothing to verify such a server against, and a name is not an
    /// address.
    #[test]
    fn a_connect_request_naming_nothing_in_the_list_opens_no_socket() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(NetPlugin::listening().over_plaintext())
            .insert_resource(ServerList::Ready(Vec::new()));
        app.update();

        app.world_mut().write_message(ConnectRequest {
            name: "somewhere-else".to_owned(),
        });
        app.update();
        app.update();

        assert!(
            matches!(state(&app), ConnectionState::Rejected { .. }),
            "a server nobody listed was dialled: {:?}",
            state(&app)
        );
        assert!(
            !app.world().contains_resource::<NetLink>(),
            "a network thread was started for a server that is in no list"
        );
    }

    #[test]
    fn a_server_that_answers_with_a_different_token_makes_a_new_character() {
        // What happens when a token is presented to a server that never issued it —
        // a different server, or one that has forgotten. The client stores whatever
        // came back, because the server is the only source of tokens there is.
        let scratch = Scratch::new("net-reissued");
        let identity = scratch.join("identity");
        std::fs::write(&identity, [0x11; PLAYER_TOKEN_LEN]).expect("a writable directory");

        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let mut app = headless_with_identity(&addr, &identity);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        assert_eq!(*app.world().resource::<Identity>(), Identity::New);
        assert_eq!(
            std::fs::read(&identity).expect("the file is still there"),
            DEFAULT_TOKEN.to_vec(),
            "the welcome's token replaced the one that was presented"
        );
    }

    #[test]
    fn a_wrong_length_identity_file_is_a_new_character_rather_than_a_failure() {
        // Losing the file must never cost the session: the server mints a fresh
        // identity, which is the only honest outcome once the bytes are not a token.
        let scratch = Scratch::new("net-corrupt");
        let identity = scratch.join("identity");
        std::fs::write(&identity, [0x11; 7]).expect("a writable directory");

        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let mut app = headless_with_identity(&addr, &identity);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        assert_eq!(*app.world().resource::<Identity>(), Identity::New);

        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let hello = sent.first().expect("the client says hello first");
        assert_eq!(
            presented_token(hello),
            None,
            "seven bytes is not a token, so nothing was presented"
        );
        assert_eq!(
            std::fs::read(&identity).expect("the file was replaced"),
            DEFAULT_TOKEN.to_vec()
        );
    }

    #[test]
    fn a_welcome_without_a_usable_token_is_refused_over_a_real_socket() {
        // The decoder's invariant, reaching the ECS the way a zero tick rate does.
        for broken in [None, Some(vec![0x11; PLAYER_TOKEN_LEN - 1])] {
            let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
                &WelcomeWire {
                    player_token: broken.clone(),
                    ..WelcomeWire::default()
                },
            )]));

            let (mut app, _scratch) = headless(&addr);
            pump_until(&mut app, "Rejected", |app| {
                matches!(state(app), ConnectionState::Rejected { .. })
            });

            assert!(
                !app.world().contains_resource::<Session>(),
                "{broken:?} is not a session"
            );
            assert!(!app.world().contains_resource::<Identity>());
        }
    }

    #[test]
    fn the_name_the_plugin_was_given_is_the_name_on_the_wire() {
        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        // A scratch identity for the same reason `headless` keeps one: this welcome
        // reaches `Established` too, and the write that follows would otherwise land
        // in the developer's own data directory.
        let scratch = Scratch::new("net-named");
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(
                NetPlugin::as_if_listed(&addr)
                    .over_plaintext()
                    .with_player_name("thora")
                    .with_identity_path(Some(scratch.join("identity"))),
            )
            .add_systems(Update, answer_the_character_phase.after(DrainNetwork));
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let hello = sent.first().expect("the client says hello first");
        assert_eq!(announced_name(hello), "thora");
    }

    #[test]
    fn identity_and_session_remain_until_the_server_finishes_leave() {
        // A leave request is not completion. The server may still send snapshots while
        // the body lingers, and this same writer is needed if cancellation is accepted;
        // InputMode rather than channel removal keeps gameplay inert meanwhile.
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });
        assert!(app.world().contains_resource::<Identity>());

        app.world_mut().write_message(DisconnectRequest);
        app.update();

        assert!(app.world().contains_resource::<Session>());
        assert!(
            app.world().contains_resource::<Identity>(),
            "the server has not yet ended this session"
        );
        assert!(!app.world().contains_resource::<Outbound>());
        assert!(app.world().contains_resource::<SuspendedOutbound>());
    }

    #[test]
    fn a_protocol_mismatch_is_rejected_with_the_reason_preserved() {
        let detail = "server speaks protocol 1, client speaks 2";
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![encode_server_reject(
            fb::RejectReason::PROTOCOL_MISMATCH,
            detail,
        )]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(
            reason.contains("PROTOCOL_MISMATCH"),
            "the code must survive: {reason}"
        );
        assert!(reason.contains(detail), "the detail must survive: {reason}");

        // No panic, no silent exit: the app is still there, and still has no
        // session to pretend otherwise.
        app.update();
        assert!(!app.world().contains_resource::<Session>());
    }

    #[test]
    fn a_full_server_is_rejected_with_its_own_reason() {
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![encode_server_reject(
            fb::RejectReason::SERVER_FULL,
            "the realm is full",
        )]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(reason.contains("SERVER_FULL"), "got {reason}");
    }

    #[test]
    fn a_server_that_answers_nothing_is_rejected_rather_than_hanging() {
        let (addr, _stub) = spawn_stub(Reply::Close);

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(
            reason.contains("before answering the handshake"),
            "got {reason}"
        );
    }

    #[test]
    fn a_peer_that_is_not_voxelheim_is_rejected() {
        // Well-framed bytes carrying something that is not an Envelope: the shape
        // of talking to the wrong service on the right port.
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![b"HTTP/1.1 200 OK\r\n\r\n".to_vec()]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(
            reason.contains("not speaking the Voxelheim protocol"),
            "got {reason}"
        );
    }

    #[test]
    fn a_payload_before_the_welcome_is_rejected() {
        // A snapshot is a legitimate message in the wrong place. schemas/
        // handshake.fbs is explicit: anything before the welcome means the peer
        // does not speak this protocol.
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![encode_entity_snapshot(
            7,
            &[EntityStateWire::at(1, 0.5)],
        )]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(reason.contains("EntitySnapshot"), "got {reason}");
    }

    #[test]
    fn a_snapshot_after_the_welcome_leaves_the_session_alone() {
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![
            encode_server_welcome(&WelcomeWire::default()),
            encode_entity_snapshot(7, &[EntityStateWire::at(1, 0.5)]),
        ]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        // Give the snapshot every chance to break something it should not.
        for _ in 0..64 {
            app.update();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(state(&app), ConnectionState::Connected);
    }

    /// A resident description crosses the thread boundary intact.
    ///
    /// **The arm this replaces dropped it on the floor**, deliberately, because until #458
    /// there was no inbox to put one in. That is exactly the kind of change nothing else
    /// notices: the payload decoded, the session stayed up, and a villager simply never got
    /// a name. Driven through a real socket rather than by pushing at the inbox, because
    /// what is under test is the router arm and not the queue.
    #[test]
    fn a_resident_appearance_after_the_welcome_reaches_the_resident_inbox() {
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![
            encode_server_welcome(&WelcomeWire::default()),
            encode_resident_appearance(
                (1 << 62) | 55,
                Some("Bjorn"),
                fb::ResidentRole::Smith.0,
                Some(AppearanceWire::default()),
            ),
        ]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "a resident appearance", |app| {
            app.world().resource::<ResidentInbox>().pending() == 1
        });

        let arrived = app.world_mut().resource_mut::<ResidentInbox>().take();
        assert_eq!(arrived.len(), 1);
        assert_eq!(arrived[0].entity_id, (1 << 62) | 55);
        assert_eq!(arrived[0].name, "Bjorn");
        assert_eq!(arrived[0].role, ResidentRole::Smith);
    }

    #[test]
    fn an_inventory_after_the_welcome_reaches_the_inventory_inbox() {
        let welcome = WelcomeWire {
            inventory_slots: 2,
            hotbar_slots: 1,
            equipment_slots: 1,
            ..WelcomeWire::default()
        };
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![
            encode_server_welcome(&welcome),
            encode_inventory_state(Some(&[1, 4, 3, 2])),
        ]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "an inventory state", |app| {
            app.world().resource::<InventoryInbox>().pending() == 1
        });

        assert_eq!(
            app.world_mut().resource_mut::<InventoryInbox>().take(),
            vec![InventoryState {
                stacks: vec![
                    InventoryStack {
                        item_id: 1,
                        count: 4,
                        ..Default::default()
                    },
                    InventoryStack {
                        item_id: 3,
                        count: 2,
                        ..Default::default()
                    },
                ],
                silver: 0,
            }]
        );
        assert_eq!(state(&app), ConnectionState::Connected);
    }

    #[test]
    fn mining_progress_after_the_welcome_reaches_its_inbox() {
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![
            encode_server_welcome(&WelcomeWire::default()),
            encode_mine_progress(Some([3, 70, -1]), 128),
        ]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "a mining progress report", |app| {
            app.world().resource::<MineProgressInbox>().pending() == 1
        });

        assert_eq!(
            app.world_mut().resource_mut::<MineProgressInbox>().take(),
            vec![MineProgress {
                pos: BlockCoord { x: 3, y: 70, z: -1 },
                progress: 128,
            }]
        );
        assert_eq!(state(&app), ConnectionState::Connected);
    }

    #[test]
    fn chunks_that_follow_the_welcome_reach_the_world_inbox_in_order() {
        // The whole streaming path over a real socket: the server's own message
        // order — unloads before loads, nearest chunk first — has to survive the
        // frame decoder, the channel and the drain, because an unload applied after
        // the load it precedes would delete a chunk the player can see.
        let coord = [0, 2, 0];
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![
            encode_server_welcome(&WelcomeWire::default()),
            encode_chunk_unload(coord),
            encode_chunk_data(coord, &[1u16, 32768]),
            encode_chunk_data([1, 2, 0], &[0u16, 32768]),
        ]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "three world updates", |app| {
            app.world().resource::<WorldInbox>().pending() == 3
        });

        let updates = app.world_mut().resource_mut::<WorldInbox>().take();
        assert_eq!(
            updates,
            vec![
                WorldUpdate::Unload {
                    coord: ChunkCoord {
                        cx: 0,
                        cy: 2,
                        cz: 0
                    }
                },
                WorldUpdate::Chunk {
                    coord: ChunkCoord {
                        cx: 0,
                        cy: 2,
                        cz: 0
                    },
                    runs: vec![1, 32768],
                },
                WorldUpdate::Chunk {
                    coord: ChunkCoord {
                        cx: 1,
                        cy: 2,
                        cz: 0
                    },
                    runs: vec![0, 32768],
                },
            ]
        );
        assert_eq!(state(&app), ConnectionState::Connected);
    }

    #[test]
    fn a_chunk_before_the_welcome_is_rejected() {
        // chunk_size arrives in the welcome. A chunk that precedes it cannot be
        // expanded, so it is a peer that does not speak this protocol.
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![encode_chunk_data(
            [0, 0, 0],
            &[1u16, 32768],
        )]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(reason.contains("ChunkData"), "got {reason}");
        assert!(
            app.world().resource::<WorldInbox>().pending() == 0,
            "nothing a refused peer sent may be applied"
        );
    }

    #[test]
    fn a_welcome_the_contract_forbids_is_rejected_over_a_real_socket() {
        // The invariant test that matters most end to end: a server that says
        // "tick rate zero" must reach the player as a protocol error, not as a
        // division inside the client.
        let welcome = WelcomeWire {
            tick_rate: 0,
            ..WelcomeWire::default()
        };
        let (addr, _stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(&welcome)]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Rejected", |app| {
            matches!(state(app), ConnectionState::Rejected { .. })
        });

        let ConnectionState::Rejected { reason } = state(&app) else {
            unreachable!("the loop above only exits on Rejected");
        };
        assert!(reason.contains("tick rate"), "got {reason}");
        assert!(
            !app.world().contains_resource::<Session>(),
            "an invalid welcome must not become a session"
        );
    }

    #[test]
    fn player_input_reaches_the_server_over_a_real_socket() {
        // The outbound half of the boundary, end to end and with nothing faked in between: a
        // Bevy system originates a frame, the writer thread puts it on the socket, and a peer
        // speaking the server's framing reads it back as a PlayerInput.
        //
        // The player plugin is what originates it, so this builds both — which is also the
        // only place the two halves are exercised against each other without a real server.
        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let scratch = Scratch::new("net-outbound");
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_plugins(
                NetPlugin::as_if_listed(&addr)
                    .over_plaintext()
                    .with_identity_path(Some(scratch.join("identity"))),
            )
            .add_plugins(crate::player::PlayerPlugin)
            .add_systems(Update, answer_the_character_phase.after(DrainNetwork));

        // Long enough for the 20 Hz cadence to fire several times.

        pump_until(&mut app, "input frames to reach the stub", |app| {
            app.world()
                .resource::<crate::player::PlayerStats>()
                .inputs_sent
                >= 3
        });
        drop(app);

        let sent = stub.join().expect("the stub thread must not panic");
        assert!(sent.len() >= 4, "the stub read {} frames", sent.len());

        assert_eq!(
            super::codec::decode(&sent[0]),
            Ok(super::codec::Message::ClientOnly("ClientHello")),
            "the handshake still goes first, and from the reader thread"
        );
        // And the choice second, from that same thread: the writer does not exist until
        // the welcome, which is what keeps one writer on this socket through a handshake
        // that waits for a person. Everything after it is input, and input is the only
        // thing the ECS originates.
        assert_eq!(
            super::codec::decode(&sent[1]),
            Ok(super::codec::Message::ClientOnly("SelectCharacterRequest")),
            "the character the screen chose is what answers the list"
        );

        let mut ticks = Vec::new();
        for frame in &sent[2..] {
            assert_eq!(
                super::codec::decode(frame),
                Ok(super::codec::Message::ClientOnly("PlayerInput")),
                "the client sent something other than input after the handshake"
            );

            let envelope = fb::root_as_envelope(frame).expect("a frame the client encoded");
            ticks.push(
                envelope
                    .payload_as_player_input()
                    .expect("a PlayerInput")
                    .client_tick(),
            );
        }

        // Strictly increasing, because the server discards anything that is not newer than
        // the last tick it accepted.
        assert!(
            ticks.windows(2).all(|pair| pair[1] > pair[0]),
            "client ticks were not increasing: {ticks:?}"
        );
        assert_eq!(
            ticks[0], 1,
            "the first input is tick 1, so 0 means 'never sent'"
        );
    }

    #[test]
    fn dropping_the_app_stops_the_net_thread() {
        let (addr, stub) = spawn_stub(Reply::AfterAChoice(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });
        drop(app);

        // The stub only returns once its socket has closed, so a join that
        // completes is the proof: the net thread noticed and let go. A leaked
        // thread would park here until PATIENCE expires and fail the assertion.
        let joined = Instant::now();
        let sent = stub.join().expect("the stub thread must not panic");
        assert!(!sent.is_empty(), "the stub read a ClientHello");
        assert!(
            joined.elapsed() < PATIENCE,
            "the net thread outlived the app"
        );
    }

    // ---------------------------------------------------------------------
    // A net thread that vanishes without reporting an ending.
    //
    // `NetPlugin` always spawns a real thread, and that thread always sends a
    // terminal event before it returns — so the case below is unreachable through
    // it. A thread that *panics* reaches it, and so does any future exit path that
    // forgets its `Ended`. These tests therefore drive `drain_session_events`
    // against a hand-made channel pair, which is the only way to decide exactly
    // when the sending end disappears.
    // ---------------------------------------------------------------------

    fn params() -> SessionParams {
        SessionParams {
            entity_id: 4,
            spawn: [0.5, 80.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: ANY_TOKEN,
            clock: Default::default(),
        }
    }

    /// An app running only the drain system, plus the sender the test controls.
    fn app_with_manual_link(initial: ConnectionState) -> (App, Sender<SessionEvent>) {
        let (event_tx, event_rx) = mpsc::channel();
        // The command receiver is dropped straight away: there is no thread to
        // instruct, and `Channels::drop` tolerates the failed send by design.
        let (command_tx, _) = mpsc::channel();

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(initial)
            .init_resource::<WorldInbox>()
            .init_resource::<SnapshotInbox>()
            .init_resource::<InventoryInbox>()
            .init_resource::<LearnedMountsInbox>()
            .init_resource::<PlayerTradeInbox>()
            .init_resource::<MineProgressInbox>()
            .init_resource::<AppearanceInbox>()
            .init_resource::<ResidentInbox>()
            .init_resource::<RefusalInbox>()
            .init_resource::<StormInbox>()
            .init_resource::<WardsInbox>()
            .init_resource::<SessionEndingInbox>()
            .insert_resource(NetLink(Mutex::new(Channels {
                events: event_rx,
                commands: command_tx,
            })))
            // Read by `rejoin_for_a_character`, which some of these tests add.
            .insert_resource(SessionSettings {
                player_name: DEFAULT_PLAYER_NAME.to_owned(),
                identity_path: None,
                data_home: None,
                transport: session::Transport::Plaintext,
            })
            .add_systems(Update, drain_session_events);

        (app, event_tx)
    }

    #[test]
    fn a_storm_warning_keeps_the_instant_the_net_thread_received_it() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connected);
        let at = Instant::now();
        let warning = StormWarning {
            phase: StormPhase::Raging,
            seconds_until: 299,
        };

        events
            .send(SessionEvent::StormWarning { warning, at })
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            app.world_mut().resource_mut::<StormInbox>().take(),
            vec![(warning, at)]
        );
    }

    #[test]
    fn a_ward_list_crosses_the_net_boundary_whole() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connected);
        let wards = WardsNearby {
            columns: vec![WardedColumn {
                cx: -2,
                cz: 3,
                kind: WardKind::Runestone,
                mine: true,
            }],
        };

        events
            .send(SessionEvent::WardsNearby(wards.clone()))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            app.world_mut().resource_mut::<WardsInbox>().take(),
            vec![wards]
        );
    }

    #[test]
    fn player_trade_state_and_close_cross_the_boundary_in_wire_order() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connected);
        let state = PlayerTradeState {
            partner_entity_id: 11,
            partner_name: "Eirik".to_owned(),
            revision: 4,
            my_offer: Vec::new(),
            their_offer: Vec::new(),
            my_silver: 2,
            their_silver: 3,
            my_confirmed: false,
            their_confirmed: true,
        };
        let closed = PlayerTradeClosed {
            partner_entity_id: 11,
            reason: PlayerTradeCloseReason::Cancelled,
        };

        events
            .send(SessionEvent::PlayerTradeState(state.clone()))
            .expect("the app holds the receiver");
        events
            .send(SessionEvent::PlayerTradeClosed(closed))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            app.world_mut().resource_mut::<PlayerTradeInbox>().take(),
            vec![
                PlayerTradeEvent::State(state),
                PlayerTradeEvent::Closed(closed)
            ]
        );
    }

    #[test]
    fn a_new_session_discards_an_unread_ward_answer_from_the_previous_one() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connecting);
        let stale = WardsNearby {
            columns: vec![WardedColumn {
                cx: -2,
                cz: 3,
                kind: WardKind::Settlement,
                mine: false,
            }],
        };
        let current = WardsNearby {
            columns: vec![WardedColumn {
                cx: 4,
                cz: 5,
                kind: WardKind::Runestone,
                mine: true,
            }],
        };
        app.world_mut().resource_mut::<WardsInbox>().push(stale);
        events
            .send(SessionEvent::Established {
                params: params(),
                returning: Some(false),
            })
            .expect("the app holds the receiver");
        events
            .send(SessionEvent::WardsNearby(current.clone()))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            app.world_mut().resource_mut::<WardsInbox>().take(),
            vec![current]
        );
    }

    #[test]
    fn a_net_thread_that_vanishes_after_the_handshake_ends_the_session() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connecting);

        events
            .send(SessionEvent::Established {
                params: params(),
                returning: Some(false),
            })
            .expect("the app holds the receiver");
        app.update();
        assert_eq!(state(&app), ConnectionState::Connected);

        // The thread dies without saying why. All the ECS sees is a closed channel.
        drop(events);
        app.update();

        assert_eq!(
            state(&app),
            ConnectionState::Disconnected,
            "a session whose thread has died is not a session; the status line \
             must not keep claiming a live connection"
        );
    }

    #[test]
    fn the_server_s_leave_acknowledgement_owns_the_visible_countdown() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connected);
        app.world_mut().insert_resource(Session(params()));
        app.world_mut().insert_resource(Identity::New);

        events
            .send(SessionEvent::Leaving(codec::LeaveStarted {
                remaining_ms: 10_000,
            }))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            state(&app),
            ConnectionState::Leaving {
                seconds_remaining: Some(10),
            }
        );
        assert!(app.world().contains_resource::<LeaveCountdown>());
        assert!(
            app.world().contains_resource::<Session>(),
            "the server has not completed leave while its socket is open"
        );

        events
            .send(SessionEvent::Ended(None))
            .expect("the app holds the receiver");
        app.update();
        assert_eq!(state(&app), ConnectionState::Disconnected);
        assert!(!app.world().contains_resource::<LeaveCountdown>());
        assert!(!app.world().contains_resource::<Session>());
        assert!(!app.world().contains_resource::<Identity>());
    }

    #[test]
    fn a_vanished_thread_ends_a_session_that_never_got_past_connecting() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connecting);
        drop(events);
        app.update();

        assert_eq!(state(&app), ConnectionState::Disconnected);
    }

    #[test]
    fn a_vanished_thread_does_not_overwrite_a_rejection() {
        // `Rejected` is terminal *and* carries the reason the player is reading.
        // Correcting it to a bare `Disconnected` would throw that away.
        let (mut app, events) = app_with_manual_link(ConnectionState::Connecting);

        events
            .send(SessionEvent::Refused(
                "SERVER_FULL: the realm is full".to_owned(),
            ))
            .expect("the app holds the receiver");
        app.update();
        drop(events);
        app.update();
        app.update();

        assert_eq!(
            state(&app),
            ConnectionState::Rejected {
                reason: "SERVER_FULL: the realm is full".to_owned()
            }
        );
    }

    /// A retryable name answer and the orderly close behind it are one exchange,
    /// even when both are waiting in the channel on the same frame. Neither the
    /// explicit `Ended` nor the sender disappearing may replace the form with a
    /// terminal state before the one redial consumes `Rejoining`.
    #[test]
    fn a_name_rejection_and_its_close_keep_the_creation_form_mounted() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Choosing);
        let mut choice = CharacterChoice::for_a_test(Vec::new(), 3).already_answered();
        choice.attempted = Some(CharacterAttempt::Create);
        app.insert_resource(choice);

        events
            .send(SessionEvent::ServerRefused(Reject {
                code: "CHARACTER_NAME_TAKEN",
                detail: "choose another name".to_owned(),
            }))
            .expect("the app holds the receiver");
        events
            .send(SessionEvent::Ended(None))
            .expect("the app holds the receiver");
        drop(events);
        app.update();

        assert_eq!(state(&app), ConnectionState::Choosing);
        let choice = app
            .world()
            .get_resource::<CharacterChoice>()
            .expect("the same creation form stays mounted");
        assert!(
            choice
                .creation_refusal()
                .is_some_and(|reason| reason.contains("choose another name")),
            "the form carries the answer the player can act on"
        );
        assert!(
            app.world().contains_resource::<Rejoining>(),
            "the clean close must leave the single redial scheduled"
        );
        assert!(
            !app.world().contains_resource::<NetLink>(),
            "the closed thread no longer owns the route"
        );
    }

    #[test]
    fn a_disconnect_request_starts_an_authoritative_leave_without_ending_the_app() {
        let (_event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let (outbound, _sent) = Outbound::to_a_test(1);

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<DisconnectRequest>()
            .insert_resource(ConnectionState::Connected)
            .insert_resource(Session(params()))
            .insert_resource(outbound)
            .insert_resource(NetLink(Mutex::new(Channels {
                events: event_rx,
                commands: command_tx,
            })))
            .add_systems(Update, disconnect_on_request);

        // A duplicate in one frame is still a single user action at the network edge.
        app.world_mut().write_message(DisconnectRequest);
        app.world_mut().write_message(DisconnectRequest);
        app.update();

        assert!(matches!(command_rx.try_recv(), Ok(NetCommand::Leave)));
        assert!(
            command_rx.try_recv().is_err(),
            "one frame emitted more than one disconnect command"
        );
        assert_eq!(
            state(&app),
            ConnectionState::Leaving {
                seconds_remaining: None,
            }
        );
        assert!(app.world().contains_resource::<Session>());
        assert!(!app.world().contains_resource::<Outbound>());
        assert!(app.world().contains_resource::<SuspendedOutbound>());
        assert!(app.world().contains_resource::<Rejoining>());
        assert!(
            app.should_exit().is_none(),
            "disconnect must leave the client running"
        );
    }

    #[test]
    fn a_disconnect_before_the_welcome_stops_the_thread_instead_of_requesting_a_leave() {
        for initial in [ConnectionState::Handshaking, ConnectionState::Choosing] {
            let (_event_tx, event_rx) = mpsc::channel();
            let (command_tx, command_rx) = mpsc::channel();

            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .add_message::<DisconnectRequest>()
                .insert_resource(initial.clone())
                .insert_resource(NetLink(Mutex::new(Channels {
                    events: event_rx,
                    commands: command_tx,
                })))
                .add_systems(Update, disconnect_on_request);

            app.world_mut().write_message(DisconnectRequest);
            app.update();

            assert!(
                matches!(command_rx.try_recv(), Ok(NetCommand::Disconnect)),
                "{initial:?} did not stop its pre-world session"
            );
            assert_eq!(state(&app), ConnectionState::Disconnected);
            assert!(app.world().contains_resource::<Rejoining>());
        }
    }

    #[test]
    fn a_second_disconnect_does_not_erase_the_server_s_leave_countdown() {
        let (_event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<DisconnectRequest>()
            .insert_resource(ConnectionState::Leaving {
                seconds_remaining: Some(7),
            })
            .insert_resource(NetLink(Mutex::new(Channels {
                events: event_rx,
                commands: command_tx,
            })))
            .add_systems(Update, disconnect_on_request);

        app.world_mut().write_message(DisconnectRequest);
        app.update();

        assert_eq!(
            state(&app),
            ConnectionState::Leaving {
                seconds_remaining: Some(7),
            }
        );
        assert!(
            command_rx.try_recv().is_err(),
            "a second disconnect was mistaken for cancellation"
        );
    }

    #[test]
    fn cancellation_asks_once_and_keeps_the_client_inert_until_the_answer() {
        let (_event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<CancelLeaveRequest>()
            .insert_resource(ConnectionState::Leaving {
                seconds_remaining: Some(7),
            })
            .insert_resource(LeaveCancellation::Available)
            .insert_resource(NetLink(Mutex::new(Channels {
                events: event_rx,
                commands: command_tx,
            })))
            .add_systems(Update, cancel_leave_on_request);

        app.world_mut().write_message(CancelLeaveRequest);
        app.world_mut().write_message(CancelLeaveRequest);
        app.update();

        assert!(matches!(command_rx.try_recv(), Ok(NetCommand::CancelLeave)));
        assert!(command_rx.try_recv().is_err());
        assert_eq!(
            state(&app),
            ConnectionState::Leaving {
                seconds_remaining: Some(7),
            },
            "asking is not an authoritative resumption"
        );
        assert_eq!(
            *app.world().resource::<LeaveCancellation>(),
            LeaveCancellation::Pending
        );
    }

    #[test]
    fn only_an_accepted_cancellation_restores_the_live_session() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Leaving {
            seconds_remaining: Some(7),
        });
        let (outbound, _sent) = Outbound::to_a_test(1);
        app.insert_resource(LeaveCancellation::Pending);
        app.insert_resource(SuspendedOutbound(outbound));
        app.insert_resource(Session(params()));
        app.insert_resource(Rejoining);
        app.insert_resource(LeaveCountdown {
            deadline: Instant::now() + Duration::from_secs(7),
        });

        events
            .send(SessionEvent::LeaveCancellation(codec::LeaveCancelResult {
                accepted: true,
                remaining_ms: 0,
            }))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(state(&app), ConnectionState::Connected);
        assert!(app.world().contains_resource::<Session>());
        assert!(app.world().contains_resource::<Outbound>());
        assert!(!app.world().contains_resource::<SuspendedOutbound>());
        assert!(!app.world().contains_resource::<LeaveCountdown>());
        assert!(!app.world().contains_resource::<LeaveCancellation>());
        assert!(!app.world().contains_resource::<Rejoining>());
    }

    #[test]
    fn an_accepted_cancellation_without_a_suspended_sender_ends_the_session() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Leaving {
            seconds_remaining: Some(7),
        });
        app.insert_resource(LeaveCancellation::Pending);
        app.insert_resource(Session(params()));
        app.insert_resource(Rejoining);

        events
            .send(SessionEvent::LeaveCancellation(codec::LeaveCancelResult {
                accepted: true,
                remaining_ms: 0,
            }))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(state(&app), ConnectionState::Disconnected);
        assert!(!app.world().contains_resource::<Session>());
        assert!(!app.world().contains_resource::<Outbound>());
        assert!(!app.world().contains_resource::<LeaveCancellation>());
        assert!(!app.world().contains_resource::<Rejoining>());
    }

    #[test]
    fn a_refused_cancellation_keeps_the_server_s_countdown_and_says_so() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Leaving {
            seconds_remaining: Some(7),
        });
        app.insert_resource(LeaveCancellation::Pending);
        app.insert_resource(Session(params()));

        events
            .send(SessionEvent::LeaveCancellation(codec::LeaveCancelResult {
                accepted: false,
                remaining_ms: 6_250,
            }))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(
            state(&app),
            ConnectionState::Leaving {
                seconds_remaining: Some(7),
            }
        );
        assert_eq!(
            *app.world().resource::<LeaveCancellation>(),
            LeaveCancellation::Refused
        );
        assert!(app.world().contains_resource::<LeaveCountdown>());
    }

    #[test]
    fn a_close_while_cancellation_is_pending_wins_the_race() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Leaving {
            seconds_remaining: Some(1),
        });
        app.insert_resource(LeaveCancellation::Pending);
        app.insert_resource(Session(params()));

        events
            .send(SessionEvent::Ended(None))
            .expect("the app holds the receiver");
        app.update();

        assert_eq!(state(&app), ConnectionState::Disconnected);
        assert!(!app.world().contains_resource::<Session>());
    }

    /// Records what a consumer with change detection would have seen, one entry
    /// per frame.
    #[derive(Resource, Default)]
    struct ChangeLog(Vec<bool>);

    fn log_state_changes(state: Res<ConnectionState>, mut log: ResMut<ChangeLog>) {
        log.0.push(state.is_changed());
    }

    #[test]
    fn a_closed_channel_corrects_the_state_once_and_then_stops_touching_it() {
        // The other half of the guard, and it regresses independently: the
        // `Disconnected` arm of the `matches!` is what makes the correction
        // idempotent. Without it the assignment happens on every frame for the
        // rest of the app's life, because this arm is reached on every frame once
        // the channel is closed — and `*state = ...` marks the resource changed on
        // every `DerefMut` whether or not the value differs. Every consumer
        // filtering on change would then re-run forever.
        //
        // Change detection is observed from inside a system rather than with
        // `is_changed()` from outside, because `App::update()` ends each frame with
        // `World::clear_trackers()`; an external check after an update is always
        // false and would make this test pass no matter what the guard did.
        let (mut app, events) = app_with_manual_link(ConnectionState::Connecting);
        app.init_resource::<ChangeLog>()
            .add_systems(Update, log_state_changes.after(drain_session_events));

        events
            .send(SessionEvent::Established {
                params: params(),
                returning: Some(false),
            })
            .expect("the app holds the receiver");
        app.update();
        assert_eq!(state(&app), ConnectionState::Connected);

        // Ignore the frames that legitimately changed the resource; what follows
        // is only the closed-channel behaviour.
        drop(events);
        app.world_mut().resource_mut::<ChangeLog>().0.clear();

        app.update();
        app.update();
        app.update();

        assert_eq!(
            app.world().resource::<ChangeLog>().0,
            vec![true, false, false],
            "the correction must happen exactly once; after that a closed channel \
             is silent"
        );
        assert_eq!(state(&app), ConnectionState::Disconnected);
    }
}

#[cfg(test)]
mod sign_in_tests {
    use super::*;
    use crate::net::codec::{SESSION_TICKET_LEN, SessionTicket};
    use crate::net::session::Scratch;
    use crate::net::tickets::{self, CachedTicket};
    use std::net::TcpListener;
    use std::time::Duration;

    /// A loopback port nothing is listening on, so `start` fails fast and
    /// deterministically. Binding and dropping is the only way to be sure a port is
    /// free rather than guessing at a number another test might be using.
    fn closed_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        drop(listener);
        port
    }

    fn app_at(authority: &str, path: PathBuf) -> (App, Receiver<String>) {
        let service = AccountService::plaintext(&format!("http://{authority}"))
            .expect("an account service URL");
        let (browser_tx, browser_rx) = mpsc::channel();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(
            SignInPlugin::new(service)
                .with_ticket_path(path)
                .with_captured_browser(browser_tx),
        );
        (app, browser_rx)
    }

    /// An app pointed at a port nothing is listening on, which is what every test
    /// that never presses the control wants.
    fn app_with(path: PathBuf) -> (App, Receiver<String>) {
        app_at(&format!("127.0.0.1:{}", closed_port()), path)
    }

    fn state(app: &App) -> SignInState {
        app.world().resource::<SignInState>().clone()
    }

    #[test]
    fn a_live_cached_ticket_signs_in_with_no_browser() {
        let scratch = Scratch::new("plugin-live");
        let path = scratch.join("service");
        let live = tickets::now_unix() + 3600;
        tickets::write(
            &path,
            CachedTicket::new(SessionTicket::from_bytes([0x5a; SESSION_TICKET_LEN]), live),
        )
        .expect("a cached ticket");

        let (mut app, browser) = app_with(path);
        app.update();

        assert_eq!(state(&app), SignInState::SignedIn);
        assert!(
            browser.try_recv().is_err(),
            "a live ticket must open no browser at all"
        );
    }

    #[test]
    fn an_expired_ticket_goes_back_to_the_login_screen_with_a_line_saying_why() {
        let scratch = Scratch::new("plugin-expired");
        let path = scratch.join("service");
        tickets::write(
            &path,
            CachedTicket::new(
                SessionTicket::from_bytes([0x5a; SESSION_TICKET_LEN]),
                tickets::now_unix() - 1,
            ),
        )
        .expect("a cached ticket");

        let (mut app, _browser) = app_with(path);
        app.update();

        match state(&app) {
            SignInState::SignedOut { reason: Some(line) } => {
                assert!(line.contains("expired"), "{line}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_first_launch_asks_without_explaining_anything() {
        // `None` rather than a sentence: nothing has gone wrong, and a reason on a
        // first launch would be a client apologising for its own default.
        let scratch = Scratch::new("plugin-first");
        let (mut app, _browser) = app_with(scratch.join("service"));
        app.update();
        assert_eq!(state(&app), SignInState::SignedOut { reason: None });
    }

    #[test]
    fn a_cache_that_is_not_a_ticket_is_a_first_launch_too() {
        let scratch = Scratch::new("plugin-garbage");
        let path = scratch.join("service");
        std::fs::write(&path, b"not a ticket").expect("a scratch file");

        let (mut app, _browser) = app_with(path);
        app.update();
        assert_eq!(state(&app), SignInState::SignedOut { reason: None });
    }

    #[test]
    fn pressing_the_control_says_a_tab_is_opening() {
        // Pointed at a service that accepts and then says nothing, so the attempt
        // is genuinely in flight while this asserts. **A closed port raced**, and
        // the race is worth writing down: Bevy inserts a sync point between two
        // chained systems, so a `Connection refused` could be reported and drained
        // in the very frame the press was made — which is correct behaviour and an
        // untestable assertion.
        let stalled = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let authority = stalled.local_addr().expect("an address").to_string();
        let scratch = Scratch::new("plugin-waiting");
        let (mut app, _browser) = app_at(&authority, scratch.join("service"));
        app.update();

        app.world_mut().write_message(SignInRequest);
        app.update();

        assert_eq!(state(&app), SignInState::Waiting);
        assert!(
            app.world().get_resource::<SignInLink>().is_some(),
            "an attempt is running"
        );
    }

    #[test]
    fn an_unreachable_service_leaves_the_screen_up_with_a_reason() {
        let scratch = Scratch::new("plugin-press");
        let path = scratch.join("service");
        let (mut app, browser) = app_with(path.clone());
        app.update();
        assert_eq!(state(&app), SignInState::SignedOut { reason: None });

        app.world_mut().write_message(SignInRequest);

        // What matters is that the attempt comes back to the login screen with a
        // reason rather than staying on "waiting" for ever.
        let mut settled = None;
        for _ in 0..200 {
            app.update();
            if let SignInState::SignedOut { reason: Some(line) } = state(&app) {
                settled = Some(line);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let reason = settled.expect("the attempt reports why it failed");
        assert!(reason.contains("127.0.0.1"), "{reason}");
        assert!(
            browser.try_recv().is_err(),
            "no browser opens when the account service cannot be reached"
        );
        assert_eq!(tickets::read(&path).0, None, "and nothing is cached");

        // The control is live again: the resource that says an attempt is running
        // is gone, so another press starts another one.
        assert!(app.world().get_resource::<SignInLink>().is_none());
    }

    #[test]
    fn a_second_press_while_an_attempt_is_running_starts_nothing() {
        // Asserted against the guard rather than against a running thread, and
        // deliberately: a test that pressed twice against a real attempt would be
        // racing whatever that attempt did next — which is exactly how the first
        // version of this test failed, on a `Connection refused` that arrived
        // between the two presses.
        let scratch = Scratch::new("plugin-twice");
        let (mut app, browser) = app_with(scratch.join("service"));
        app.update();

        *app.world_mut().resource_mut::<SignInState>() = SignInState::Waiting;
        app.world_mut().write_message(SignInRequest);
        app.update();

        assert_eq!(state(&app), SignInState::Waiting);
        assert!(
            app.world().get_resource::<SignInLink>().is_none(),
            "no second attempt was started"
        );
        assert!(browser.try_recv().is_err(), "and no second tab was opened");
    }

    #[test]
    fn a_refusal_never_carries_a_credential_into_the_state_the_ui_reads() {
        // `SignInState` is the one thing about a sign-in that leaves this module,
        // and the login screen renders it verbatim. Its `Debug` is what a log line
        // or an assertion failure would print.
        let scratch = Scratch::new("plugin-quiet");
        let (mut app, _browser) = app_with(scratch.join("service"));
        app.update();
        app.world_mut().write_message(SignInRequest);
        for _ in 0..200 {
            app.update();
            if matches!(state(&app), SignInState::SignedOut { reason: Some(_) }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let printed = format!("{:?}", state(&app));
        for forbidden in ["ticket", "secret", "code="] {
            assert!(
                !printed.to_ascii_lowercase().contains(forbidden),
                "{forbidden} appears in {printed}"
            );
        }
    }
}

#[cfg(test)]
mod server_list_tests {
    use super::*;
    use crate::net::codec::{SESSION_TICKET_LEN, SessionTicket};
    use crate::net::session::Scratch;
    use crate::net::tickets::{self, CachedTicket};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    /// How long a test will pump before giving up on a read that goes to a closed
    /// port. Generous because it covers a loaded CI runner, and irrelevant to runtime
    /// because a refused connection answers in microseconds.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// A loopback port nothing is listening on, so the read fails fast and
    /// deterministically. Binding and dropping is the only way to be sure a port is
    /// free rather than guessing at a number another test might be using.
    fn closed_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        drop(listener);
        port
    }

    /// An app with a live cached sign-in, pointed at an account service that is not
    /// there. Both plugins, because that is how they are built in `main.rs`: the list
    /// reads the settings the sign-in inserts.
    fn signed_in_app(scratch: &Scratch) -> App {
        let path = scratch.join("service");
        tickets::write(
            &path,
            CachedTicket::new(
                SessionTicket::from_bytes([0x5a; SESSION_TICKET_LEN]),
                tickets::now_unix() + 3600,
            ),
        )
        .expect("a cached ticket");

        let service = AccountService::plaintext(&format!("http://127.0.0.1:{}", closed_port()))
            .expect("an account service URL");
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(SignInPlugin::new(service).with_ticket_path(path))
            .add_plugins(ServerListPlugin);
        app
    }

    fn list(app: &App) -> ServerList {
        app.world().resource::<ServerList>().clone()
    }

    fn pump_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            app.update();
            if done(app) {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("timed out waiting for {what}; the list is {:?}", list(app));
    }

    /// **The acceptance criterion this whole module exists for.** An account service
    /// that cannot be reached produces a line and a retry — never `Ready(vec![])`,
    /// which the screen would draw as an empty list and a player would read as "no
    /// servers exist".
    #[test]
    fn an_unreachable_account_service_is_never_an_empty_list() {
        let scratch = Scratch::new("list-unreachable");
        let mut app = signed_in_app(&scratch);

        pump_until(&mut app, "the read to fail", |app| {
            matches!(list(app), ServerList::Unavailable(_))
        });

        match list(&app) {
            ServerList::Unavailable(reason) => assert!(!reason.is_empty(), "a silent refusal"),
            other => panic!("an unreachable service answered {other:?}"),
        }
    }

    /// The retry starts another read, which is what makes the button on the screen a
    /// button rather than a decoration.
    #[test]
    fn the_retry_asks_again() {
        let scratch = Scratch::new("list-retry");
        let mut app = signed_in_app(&scratch);
        pump_until(&mut app, "the first read to fail", |app| {
            matches!(list(app), ServerList::Unavailable(_))
        });

        // A listener that accepts the connection and then says nothing, so the second
        // read is still in flight when the assertion runs. Against the closed port
        // above it would start and finish inside one frame, and "the retry started a
        // read" would be a race rather than an assertion.
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let authority = listener.local_addr().expect("an address").to_string();
        app.world_mut().resource_mut::<SignInSettings>().service =
            AccountService::plaintext(&format!("http://{authority}"))
                .expect("an account service URL");

        app.world_mut().write_message(RefreshServerList);
        app.update();

        assert!(
            app.world().contains_resource::<ServerListLink>(),
            "the retry started no read"
        );
        assert_eq!(
            list(&app),
            ServerList::Loading,
            "the screen kept showing the failure it was retrying past"
        );
    }

    /// Nothing is read until there is a sign-in to read it with. A client sitting on
    /// the login screen has no ticket to present, and asking anyway would spend a
    /// socket to be told so.
    #[test]
    fn no_read_happens_before_the_sign_in() {
        let scratch = Scratch::new("list-signed-out");
        // No cached ticket at all, so `SignInPlugin` starts signed out.
        let service = AccountService::plaintext(&format!("http://127.0.0.1:{}", closed_port()))
            .expect("an account service URL");
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(SignInPlugin::new(service).with_ticket_path(scratch.join("service")))
            .add_plugins(ServerListPlugin);

        for _ in 0..5 {
            app.update();
        }

        assert_eq!(list(&app), ServerList::Loading);
        assert!(
            !app.world().contains_resource::<ServerListLink>(),
            "a list was read with no sign-in to read it with"
        );
    }

    /// A ticket the account service will not accept sends the player back to the login
    /// screen rather than leaving a retry that cannot work — and the list goes back to
    /// `Loading`, so signing in again reads it without a press.
    #[test]
    fn a_ticket_that_will_not_do_puts_the_login_screen_back_up() {
        let scratch = Scratch::new("list-signed-out-mid");
        let mut app = signed_in_app(&scratch);
        app.update();

        // Delivered as the list thread would, which is the seam a real refusal
        // arrives through: what is under test is the ECS half.
        let (events, receiver) = mpsc::channel();
        events
            .send(ServerListEvent::SignedOut("sign in again".to_owned()))
            .expect("the receiver is held below");
        app.world_mut()
            .insert_resource(ServerListLink(Mutex::new(receiver)));
        app.update();

        assert_eq!(list(&app), ServerList::Loading);
        assert!(
            matches!(
                app.world().resource::<SignInState>(),
                SignInState::SignedOut { reason: Some(_) }
            ),
            "an unusable ticket left the client claiming to be signed in"
        );
    }

    /// An account service that answers with nothing is a list with nothing in it, and
    /// that is a different resource state from one that could not be read.
    #[test]
    fn an_empty_answer_is_a_ready_list_rather_than_a_failure() {
        let scratch = Scratch::new("list-empty");
        let mut app = signed_in_app(&scratch);
        app.update();

        let (events, receiver) = mpsc::channel();
        events
            .send(ServerListEvent::Ready(Vec::new()))
            .expect("the receiver is held below");
        app.world_mut()
            .insert_resource(ServerListLink(Mutex::new(receiver)));
        app.update();

        assert_eq!(list(&app), ServerList::Ready(Vec::new()));
        assert!(
            !app.world().contains_resource::<ServerListLink>(),
            "a finished read left its link behind, so no retry could start another"
        );
    }

    #[test]
    fn the_mob_hit_inbox_is_bounded_and_discards_the_oldest_presentation_event() {
        let mut inbox = MobHitInbox::default();
        for index in 0..MOB_HIT_INBOX_CAPACITY + 2 {
            inbox.push(MobHit {
                attacker_entity_id: index as u64 + 1,
                attacker_pos: [index as f32, 0.0, -1.0],
            });
        }

        assert_eq!(inbox.pending(), MOB_HIT_INBOX_CAPACITY);
        let queued = inbox.take();
        assert_eq!(queued.first().map(|hit| hit.attacker_entity_id), Some(3));
        assert_eq!(
            queued.last().map(|hit| hit.attacker_entity_id),
            Some((MOB_HIT_INBOX_CAPACITY + 2) as u64)
        );
    }
}
