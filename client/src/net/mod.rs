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
mod session;
mod signin;
mod tickets;
mod tls;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::Instant;

use bevy::prelude::*;

pub use codec::{
    ActionRefused, AttackRequest, BlockCoord, BlockEditRequest, ChunkCoord, CraftRequest,
    EditAction, EntityState, Facing, InventoryMoveRequest, InventoryStack, InventoryState,
    ItemDropState, LifeState, MineProgress, MineRequest, MobAction, MobKind, MobState,
    PlaceStructureRequest, PlayerInput, PlayerVitals, RecipeId, RefusalReason, RefusedAction,
    Reject, RemoveStructureRequest, RepairRequest, SessionParams, Snapshot, StructureKind,
    StructureState, WorldClock, WorldUpdate,
};
// `PlayerToken` itself is deliberately not re-exported: outside this module the
// token is a field nobody reads, and a name nothing outside `net` can spell is a
// name nothing outside `net` can start deciding from.
#[cfg(test)]
pub use codec::ANY_TOKEN;
pub use codec::{
    encode_attack_request, encode_block_edit_request, encode_chunk_resend_request,
    encode_craft_request, encode_inventory_move_request, encode_mine_request,
    encode_place_structure_request, encode_player_input, encode_remove_structure_request,
    encode_repair_request,
};
use session::{NetCommand, SessionEvent};
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
/// Two of the five variants are terminal-with-a-reason and terminal-without-one,
/// and the difference is worth stating: [`Self::Rejected`] means *there is no
/// session and here is why* — a `ServerReject`, an unreachable address, or a peer
/// that turned out not to speak this protocol. [`Self::Disconnected`] means a
/// session that existed has ended; by then the player has seen a world and the
/// detail belongs in the log.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Opening the socket.
    Connecting,
    /// The socket is up and `ClientHello` is on the wire.
    Handshaking,
    /// A validated `ServerWelcome` arrived. [`Session`] exists.
    Connected,
    /// There is no session. `reason` is written for a player to read.
    Rejected { reason: String },
    /// A session that existed has ended.
    Disconnected,
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
}

/// The address the client was told to connect to. Read by the UI so the status
/// line can name it; the net thread has its own copy.
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

/// Owns the socket thread and publishes what it reports.
pub struct NetPlugin {
    server_addr: String,
    player_name: String,
    identity_path: Option<PathBuf>,
    /// Which transport the session thread builds.
    ///
    /// Always [`session::Transport::Encrypted`] in a shipped client — the other variant
    /// does not exist outside `cfg(test)`, so this field cannot be set to anything else
    /// in a build a player runs. See [`session::Transport`].
    transport: session::Transport,
}

impl NetPlugin {
    /// Connects to `server_addr` when the app is built, as [`DEFAULT_PLAYER_NAME`]
    /// and with the identity file this client keeps for that address.
    pub fn new(server_addr: impl Into<String>) -> Self {
        Self {
            server_addr: server_addr.into(),
            player_name: DEFAULT_PLAYER_NAME.to_owned(),
            identity_path: None,
            transport: session::Transport::Encrypted,
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
}

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ServerAddress(self.server_addr.clone()))
            .init_resource::<WorldInbox>()
            .init_resource::<SnapshotInbox>()
            .init_resource::<InventoryInbox>()
            .init_resource::<MineProgressInbox>()
            .init_resource::<RefusalInbox>()
            .add_message::<DisconnectRequest>();

        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        // Bounded, unlike the other two: this is the only channel the ECS *produces* into,
        // and a producer that cannot block has to be able to drop. See OUTBOUND_QUEUE.
        let (outbound_tx, outbound_rx) = mpsc::sync_channel(OUTBOUND_QUEUE);
        let addr = self.server_addr.clone();
        let player_name = self.player_name.clone();
        let identity_path = self.identity_path.clone();
        let transport = self.transport;

        let spawned = thread::Builder::new()
            .name("voxelheim-net".to_owned())
            .spawn(move || {
                session::run(
                    addr,
                    player_name,
                    identity_path,
                    transport,
                    event_tx,
                    command_rx,
                    outbound_rx,
                )
            });

        match spawned {
            // The handle is dropped, detaching the thread. Joining it would mean
            // blocking app teardown on a socket; the command channel closing is
            // what tells it to stop, and it wakes to notice within one read
            // timeout.
            Ok(_detached) => {
                app.insert_resource(ConnectionState::Connecting)
                    .insert_resource(NetLink(Mutex::new(Channels {
                        events: event_rx,
                        commands: command_tx,
                    })))
                    .insert_resource(Outbound(Mutex::new(outbound_tx)))
                    .add_systems(
                        Update,
                        (
                            drain_session_events.in_set(DrainNetwork),
                            disconnect_on_request.after(DrainNetwork),
                        ),
                    );
            }
            // Not a panic: a client that cannot start a thread can still tell the
            // player so, and "no panic, no silent exit" has no exception for
            // failures that are nobody's fault.
            Err(err) => {
                error!("the network thread would not start: {err}");
                app.insert_resource(ConnectionState::Rejected {
                    reason: format!("cannot start the network thread: {err}"),
                });
            }
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
    mining: ResMut<'w, MineProgressInbox>,
    refusals: ResMut<'w, RefusalInbox>,
}

/// Applies everything the net thread has said since the last frame.
///
/// `try_recv` in a loop, never a blocking receive: this system runs on Bevy's
/// schedule and must return whether the network had anything to say or not.
fn drain_session_events(
    mut commands: Commands,
    mut link: ResMut<NetLink>,
    mut state: ResMut<ConnectionState>,
    mut inboxes: Inboxes<'_>,
) {
    // `get_mut` rather than `lock`: `ResMut` is already exclusive, so there is no
    // lock to take. Poisoning is recovered from rather than propagated — nothing
    // here panics while holding it, and a client that stopped reading its socket
    // because of an unrelated panic elsewhere would be a worse outcome than a
    // recovered mutex.
    let channels = match link.0.get_mut() {
        Ok(channels) => channels,
        Err(poisoned) => poisoned.into_inner(),
    };

    loop {
        match channels.events.try_recv() {
            Ok(SessionEvent::Handshaking) => *state = ConnectionState::Handshaking,

            Ok(SessionEvent::Established { params, returning }) => {
                // Every field but the token, which is never written down. The
                // newtype refuses to print itself, so this stays true even if a
                // later line reaches for `{params:?}`.
                info!(
                    "session established: entity {} at {:?}, seed {}, {} Hz, chunk {}, view {}, {}",
                    params.entity_id,
                    params.spawn,
                    params.world_seed,
                    params.tick_rate,
                    params.chunk_size,
                    params.view_distance,
                    if returning {
                        "a returning character"
                    } else {
                        "a new character"
                    },
                );
                commands.insert_resource(Session(params));
                commands.insert_resource(if returning {
                    Identity::Returning
                } else {
                    Identity::New
                });
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

            // Complete authoritative progress, interpreted only by the player module.
            Ok(SessionEvent::MineProgress(progress)) => inboxes.mining.0.push(progress),

            // Queued for the UI rather than interpreted here. Not logged either: the one
            // half worth a log line is a refusal that says *this build* sent something the
            // server could not read, and the status line is where that decision is made,
            // beside the sentence it writes for the other half.
            Ok(SessionEvent::ActionRefused(refused)) => inboxes.refusals.0.push(refused),

            Ok(SessionEvent::Refused(reason)) => {
                warn!("no session: {reason}");
                *state = ConnectionState::Rejected { reason };
                // Dropping the sender closes the channel, which is how the writer thread
                // learns there is nothing left to write and lets go of its socket handle.
                commands.remove_resource::<Outbound>();
                commands.remove_resource::<Session>();
                commands.remove_resource::<Identity>();
            }

            Ok(SessionEvent::Ended(detail)) => {
                match detail {
                    Some(detail) => warn!("session ended: {detail}"),
                    None => info!("the server closed the connection"),
                }
                *state = ConnectionState::Disconnected;
                commands.remove_resource::<Outbound>();
                commands.remove_resource::<Session>();
                commands.remove_resource::<Identity>();
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
                // throw that away.
                if !matches!(
                    *state,
                    ConnectionState::Rejected { .. } | ConnectionState::Disconnected
                ) {
                    *state = ConnectionState::Disconnected;
                    // Inside the guard, so this stays idempotent along with the
                    // assignment: `remove_resource` on an absent resource is a no-op, but
                    // queuing one every frame for the rest of the app's life is not free.
                    commands.remove_resource::<Outbound>();
                    commands.remove_resource::<Session>();
                    commands.remove_resource::<Identity>();
                }
                break;
            }
        }
    }
}

/// Ends the live session without closing the app.
///
/// The command channel is still owned here at the network boundary. UI code can ask, but
/// cannot touch a socket or pretend the connection ended by editing display state alone.
fn disconnect_on_request(
    mut requests: MessageReader<DisconnectRequest>,
    mut commands: Commands,
    mut link: ResMut<NetLink>,
    mut state: ResMut<ConnectionState>,
) {
    // Consume the whole frame's batch. Several UI producers asking to disconnect still
    // mean one network command, not one command replayed across several later frames.
    if requests.read().count() == 0 {
        return;
    }

    let channels = match link.0.get_mut() {
        Ok(channels) => channels,
        Err(poisoned) => poisoned.into_inner(),
    };
    let _ = channels.commands.send(NetCommand::Disconnect);

    if !matches!(
        *state,
        ConnectionState::Rejected { .. } | ConnectionState::Disconnected
    ) {
        *state = ConnectionState::Disconnected;
    }
    // Closing the writer tells its thread to release its socket handle. The read thread
    // remains represented by NetLink long enough to report its orderly end.
    commands.remove_resource::<Outbound>();
    commands.remove_resource::<Session>();
    commands.remove_resource::<Identity>();
}

/// Where a sign-in has got to, and the only thing about one that leaves this
/// module.
///
/// **The ticket itself deliberately never reaches the ECS.** It lives for the
/// length of one attempt on the sign-in thread and then only in the cache, at mode
/// `0600` — so there is no resource holding a bearer credential for a `{:?}`
/// somewhere to find, and no name outside `net` that could start deciding from
/// one. The screen that presents a ticket is the server list, and that is #107;
/// it reads the cache, which is the store, exactly as the identity file is.
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
    ticket_path: Option<PathBuf>,
    browser: signin::Browser,
}

impl SignInPlugin {
    /// Signs in against `service`, keeping the ticket in the file this client
    /// derives for it.
    pub fn new(service: AccountService) -> Self {
        Self {
            service,
            ticket_path: None,
            browser: signin::Browser::System,
        }
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
        let ticket_path = self.ticket_path.clone().or_else(|| {
            tickets::default_ticket_path(self.service.authority(), &session::Environment::read())
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
    let ticket_path = settings.ticket_path.clone();
    let browser = settings.browser.clone();

    match thread::Builder::new()
        .name("voxelheim-signin".to_owned())
        .spawn(move || signin::run(service, ticket_path, browser, event_tx, command_rx))
    {
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::codec::PLAYER_TOKEN_LEN;
    use super::codec::server_side::{
        DEFAULT_TOKEN, EntityStateWire, WelcomeWire, encode_chunk_data, encode_chunk_unload,
        encode_entity_snapshot, encode_inventory_state, encode_mine_progress, encode_server_reject,
        encode_server_welcome,
    };
    use super::frame::FRAME_HEADER_SIZE;
    use super::session::Scratch;
    use super::*;
    use crate::wire::voxelheim::net as fb;

    /// How long a test will pump the app waiting for a state. Generous because it
    /// covers a loopback round trip on a loaded CI runner, and irrelevant to
    /// runtime because every assertion is reached long before it.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// What the stub does once it has read the client's `ClientHello`.
    enum Reply {
        /// Answer with these frames, then hold the connection open until the
        /// client closes it. Holding matters: closing immediately would race a
        /// `Connected` assertion against the `Disconnected` that follows.
        Frames(Vec<Vec<u8>>),
        /// Close without answering.
        Close,
        /// Hold the connection open and say nothing.
        Hold,
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
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener
            .local_addr()
            .expect("read the stub's own address")
            .to_string();

        let handle = thread::spawn(move || {
            let mut received = Vec::new();
            let Ok((mut socket, _)) = listener.accept() else {
                return received;
            };
            socket
                .set_read_timeout(Some(PATIENCE))
                .expect("a fresh socket accepts a read timeout");

            // The hello always comes first: the server would refuse anything else.
            if let Some(frame) = read_one_frame(&mut socket) {
                received.push(frame);
            }

            match reply {
                Reply::Close => return received,
                Reply::Hold => {}
                Reply::Frames(frames) => {
                    for payload in &frames {
                        let mut framed = (payload.len() as u32).to_be_bytes().to_vec();
                        framed.extend_from_slice(payload);
                        if socket.write_all(&framed).is_err() {
                            return received;
                        }
                    }
                    if socket.flush().is_err() {
                        return received;
                    }
                }
            }

            // Read until the client hangs up, which is how the socket stays open for
            // exactly as long as the client wants it — and how everything sent after the
            // handshake is recorded.
            while let Some(frame) = read_one_frame(&mut socket) {
                received.push(frame);
            }

            received
        });

        (addr, handle)
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
        app.add_plugins(MinimalPlugins).add_plugins(
            NetPlugin::new(addr)
                .over_plaintext()
                .with_identity_path(Some(identity.to_path_buf())),
        );
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
            inventory_slots: 36,
            hotbar_slots: 9,
            ..WelcomeWire::default()
        };
        let (addr, stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(&welcome)]));

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
        let (addr, stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(&welcome)]));

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

    #[test]
    fn a_stored_token_is_presented_and_a_matching_welcome_is_a_returning_session() {
        let scratch = Scratch::new("net-returning");
        let identity = scratch.join("identity");
        std::fs::write(&identity, DEFAULT_TOKEN).expect("a writable scratch directory");

        let (addr, stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(
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

    #[test]
    fn a_server_that_answers_with_a_different_token_makes_a_new_character() {
        // What happens when a token is presented to a server that never issued it —
        // a different server, or one that has forgotten. The client stores whatever
        // came back, because the server is the only source of tokens there is.
        let scratch = Scratch::new("net-reissued");
        let identity = scratch.join("identity");
        std::fs::write(&identity, [0x11; PLAYER_TOKEN_LEN]).expect("a writable directory");

        let (addr, _stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(
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

        let (addr, stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(
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
            let (addr, _stub) =
                spawn_stub(Reply::Frames(vec![encode_server_welcome(&WelcomeWire {
                    player_token: broken.clone(),
                    ..WelcomeWire::default()
                })]));

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
        let (addr, stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        // A scratch identity for the same reason `headless` keeps one: this welcome
        // reaches `Established` too, and the write that follows would otherwise land
        // in the developer's own data directory.
        let scratch = Scratch::new("net-named");
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(
            NetPlugin::new(&addr)
                .over_plaintext()
                .with_player_name("thora")
                .with_identity_path(Some(scratch.join("identity"))),
        );
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });

        drop(app);
        let sent = stub.join().expect("the stub thread must not panic");
        let hello = sent.first().expect("the client says hello first");
        assert_eq!(announced_name(hello), "thora");
    }

    #[test]
    fn the_identity_goes_when_the_session_does() {
        // Inserted and removed exactly where `Session` is: a status line reading a
        // stale `Identity` after a disconnect would describe a session that ended.
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let (mut app, _scratch) = headless(&addr);
        pump_until(&mut app, "Connected", |app| {
            state(app) == ConnectionState::Connected
        });
        assert!(app.world().contains_resource::<Identity>());

        app.world_mut().write_message(DisconnectRequest);
        app.update();

        assert!(!app.world().contains_resource::<Session>());
        assert!(
            !app.world().contains_resource::<Identity>(),
            "the identity belongs to the session that ended"
        );
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
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![
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

    #[test]
    fn an_inventory_after_the_welcome_reaches_the_inventory_inbox() {
        let welcome = WelcomeWire {
            inventory_slots: 2,
            hotbar_slots: 2,
            ..WelcomeWire::default()
        };
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![
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
            }]
        );
        assert_eq!(state(&app), ConnectionState::Connected);
    }

    #[test]
    fn mining_progress_after_the_welcome_reaches_its_inbox() {
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![
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
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![
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
        let (addr, _stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(&welcome)]));

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
        let (addr, stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(
            &WelcomeWire::default(),
        )]));

        let scratch = Scratch::new("net-outbound");
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_plugins(
                NetPlugin::new(&addr)
                    .over_plaintext()
                    .with_identity_path(Some(scratch.join("identity"))),
            )
            .add_plugins(crate::player::PlayerPlugin);

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

        let mut ticks = Vec::new();
        for frame in &sent[1..] {
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
        let (addr, stub) = spawn_stub(Reply::Frames(vec![encode_server_welcome(
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
            inventory_slots: 36,
            hotbar_slots: 9,
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
            .init_resource::<MineProgressInbox>()
            .init_resource::<RefusalInbox>()
            .insert_resource(NetLink(Mutex::new(Channels {
                events: event_rx,
                commands: command_tx,
            })))
            .add_systems(Update, drain_session_events);

        (app, event_tx)
    }

    #[test]
    fn a_net_thread_that_vanishes_after_the_handshake_ends_the_session() {
        let (mut app, events) = app_with_manual_link(ConnectionState::Connecting);

        events
            .send(SessionEvent::Established {
                params: params(),
                returning: false,
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

    #[test]
    fn a_disconnect_request_ends_the_session_without_ending_the_app() {
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

        assert!(matches!(command_rx.try_recv(), Ok(NetCommand::Disconnect)));
        assert!(
            command_rx.try_recv().is_err(),
            "one frame emitted more than one disconnect command"
        );
        assert_eq!(state(&app), ConnectionState::Disconnected);
        assert!(!app.world().contains_resource::<Session>());
        assert!(!app.world().contains_resource::<Outbound>());
        assert!(
            app.should_exit().is_none(),
            "disconnect must leave the client running"
        );
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
                returning: false,
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
        let service =
            AccountService::parse(&format!("http://{authority}")).expect("an account service URL");
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
