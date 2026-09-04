//! The blocking half of the client's networking: everything that owns the socket.
//!
//! This module runs on its own `std::thread` and is the only code in the client
//! that blocks. No Bevy type crosses the line — it speaks in [`SessionEvent`] and
//! [`NetCommand`] over `std::sync::mpsc`, which is what lets the ECS side drain
//! with `try_recv` and never wait for a network.
//!
//! It mirrors `internal/session` on the server: one connection's lifetime, the
//! handshake that admits it, and no opinion whatsoever about what a message means
//! for the world.
//!
//! ## The identity file
//!
//! One connection's lifetime has exactly two points where an identity matters: the
//! token is read from a file just before the hello that presents it, and the one
//! the welcome answers with is written back. Both are blocking file I/O, which is
//! why they live on this thread and nowhere else — `codec` stays free of I/O, and
//! `ui`, `player` and `world` never learn that a file exists.
//!
//! **One file per server address**, because a token means nothing to a server that
//! did not mint it: presenting server A's token to server B makes a new character
//! on B, and must not overwrite what A issued. `--identity` overrides the path
//! outright, which is how one machine runs two characters against one server.
//!
//! **There is no identity file at all on a connection nobody stated an expectation
//! for.** [`run`] opens one only for [`tls::Expectation::Listed`] — a server the list
//! carried a certificate fingerprint for — because handing a bearer credential to
//! whoever answers an address is the theft the encryption exists to prevent, performed
//! by the client itself. `--server` is the other variant and it is the development
//! path: encrypted and unverified. That is not a check placed before the hello; it is
//! the shape of the two variants, so there is no ordering to get wrong.
//!
//! Nothing here decides anything from a token. It is read, presented, and stored;
//! every consequence of it belongs to the server.
//!
//! ## The ticket, which is gated differently on purpose
//!
//! A session ticket is read from its own cache file on this thread and presented in the
//! same hello, and it is **not** gated on the expectation the way the identity file is.
//! The asymmetry is #154's and the argument is on [`Target::ticket`]: the two are
//! different credentials, and only one of them is bounded enough to show an address
//! nobody vouched for. What is unchanged is the certificate — an `Unlisted` session
//! verifies nothing and says so, and a `Listed` mismatch is refused inside the
//! handshake, before either file is opened.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::codec::{
    self, ActionRefused, CharacterList, ChatMessage, InventoryState, MineProgress, MobHit,
    PLAYER_TOKEN_LEN, PartyInvite, PlayerAppearance, PlayerToken, Reject, SessionParams,
    SessionTicket, Snapshot, WorldUpdate,
};

use super::frame::{self, FrameDecoder};
use super::handshake::{Handshake, Phase, Transition};
use super::tickets;
use super::tls;

/// How long a connect attempt may take before it counts as unreachable. Without
/// it, a black-holed address parks the thread until the OS gives up, which on
/// Linux is minutes of a status line that says "Connecting".
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a read blocks before the loop looks for a shutdown command.
///
/// Short enough that closing the window does not leave a thread parked on a
/// socket nobody reads; long enough that an idle session costs five wakeups a
/// second. This is a poll interval, not a session timeout: a quiet server is
/// normal, and nothing here disconnects one.
const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Read buffer size. A handshake frame is tens of bytes; the frames that make
/// this worth sizing arrive with chunk streaming, and are bounded by
/// [`frame::MAX_FRAME_SIZE`] regardless.
const READ_BUFFER_SIZE: usize = 8 * 1024;

/// Where identity files live under the data directory, one file per server.
pub(super) const IDENTITY_DIR: &[&str] = &["voxelheim", "identity"];

/// Where the last character played on each server is remembered, one file per server.
///
/// Beside the identity files rather than in them, because the two are different kinds of
/// thing: an identity token is a bearer credential this client presents, and a character
/// id is a number the server minted, listed to this account, and will re-check on every
/// selection. Sharing a file would mean one format holding both.
pub(super) const CHARACTER_DIR: &[&str] = &["voxelheim", "characters"];

/// The XDG base directory for per-user data, when it is set to an absolute path.
///
/// Guarded the same way [`Environment::read`] is, and only because that is its one
/// reader: a name a test build has no way to look up is a name a test build has no use
/// for.
#[cfg(not(test))]
const XDG_DATA_HOME: &str = "XDG_DATA_HOME";

/// The home directory, used when `$XDG_DATA_HOME` says nothing usable.
#[cfg(not(test))]
const HOME: &str = "HOME";

/// The XDG default for `$XDG_DATA_HOME`, relative to `$HOME`.
const DEFAULT_DATA_HOME: &[&str] = &[".local", "share"];

/// Distinguishes one process's temporary identity file from another's, so two
/// clients writing the same file at once cannot land on the same temporary name.
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// What the ECS can tell the net thread.
///
/// [`Self::Disconnect`] is sent by `Drop` on the ECS side rather than by a system,
/// because "the app is going away" is an instruction a system may not be around to give.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NetCommand {
    Disconnect,
    /// Ask the authoritative server to begin leaving. Unlike `Disconnect`, this writes
    /// a wire frame and keeps the reader alive for the acknowledgement, a possible
    /// cancellation answer, and the final server close.
    Leave,
    /// Ask the server to stop a leave that this live session already began.
    CancelLeave,
    /// Which character this session plays, as the player chose it.
    ///
    /// **It travels as a command rather than as a frame, and that is what keeps one
    /// writer on this socket.** Every other client message the ECS originates goes
    /// through the outbound channel to the writer thread; this one is written by *this*
    /// thread, which is the only writer until the welcome — the same arrangement the
    /// hello already has, extended over the phase between them. It also keeps the encode
    /// beside the hello's, and it is what lets the handshake be told that a choice went
    /// out ([`Handshake::chose`]).
    Choose(Choice),
}

/// What a player chose on the character screen.
///
/// The net-thread half of `net::ChooseCharacter`: the same two answers without Bevy's
/// `Message` derive, because no Bevy type appears below `net/mod.rs`. The mapping is
/// four lines in `net/mod.rs`, the same trade [`SessionEvent::Established`]'s `bool`
/// makes for `net::Identity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Choice {
    /// Play the character this id names. It is one the server minted and listed, which
    /// is the one kind of identifier a client may echo back.
    Play(u64),
    /// Make one and play it. Creation and selection are one step in this contract.
    Create(codec::CreateCharacterRequest),
}

/// What the net thread tells the ECS.
///
/// `Handshaking`, `World`, `Inventory`, and `MineProgress` are admitted messages,
/// and `Warning` is a line for the log; all other variants terminate the session.
/// A server rejection stays typed until the ECS maps it onto `ConnectionState`: the
/// character-name codes are the one pair whose remedy is another character exchange,
/// and flattening them here used to throw that distinction away. The naming remains
/// about what *happened* rather than about what should be displayed.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "Snapshot is the hot-path event and remains inline; V27 adds sparse mount and cast projections"
)]
pub(super) enum SessionEvent {
    /// The socket is up and `ClientHello` is on the wire.
    Handshaking,
    /// The characters this account owns on this world, and which of them this client
    /// played here last.
    ///
    /// **The answer to the hello, and the phase a person is inside.** Nothing happens
    /// next until the ECS answers with a [`NetCommand::Choose`], which is what the
    /// character screen writes and what this thread turns into a frame.
    ///
    /// `played_before` is read from this client's own file rather than from anything the
    /// server said — it is a convenience, not a claim, and a stale id simply matches no
    /// row. See [`ChosenCharacter`].
    Characters {
        list: CharacterList,
        played_before: Option<u64>,
    },

    /// A validated `ServerWelcome` arrived.
    ///
    /// `returning` says whether the token in it is the one this client presented,
    /// which is the only thing the client derives from a token and is derived for
    /// the status line alone. It is not a gameplay answer: the server had already
    /// settled the identity before it sent the welcome, and both values of this
    /// flag describe a session that is equally established.
    ///
    /// **`None` is a session that kept no identity file, and it is the answer this
    /// client is entitled to give there** — not `Some(false)`. A session that presents
    /// no token has made no comparison, so "a new character" would be a claim with
    /// nothing behind it, and on the path #154 added it is a claim the server
    /// contradicts in the same handshake: a ticket names an account, the account
    /// decides which character comes back, and this client is deliberately told
    /// neither. Rendering a guess as a fact is the one thing a status line must not
    /// do.
    Established {
        params: SessionParams,
        returning: Option<bool>,
    },
    /// The server said something about the voxel world. Ordered with respect to
    /// every other event on this channel, which is what lets an unload that
    /// follows a load stay behind it.
    World(WorldUpdate),
    /// One tick of authoritative entity state, with the moment it arrived.
    ///
    /// The timestamp is taken **here**, where the bytes were decoded, rather than on the
    /// frame that consumes it. Interpolation divides by the gap between two arrivals, so
    /// a frame's worth of scheduling jitter in that number is a frame's worth of jitter
    /// in every position on screen.
    Snapshot { snapshot: Snapshot, at: Instant },
    /// The player's complete authoritative inventory.
    Inventory(InventoryState),
    /// The complete authoritative learned-mount set.
    LearnedMounts(codec::LearnedMounts),
    /// Authoritative progress for one mined voxel.
    MineProgress(MineProgress),
    /// What one visible player looks like.
    ///
    /// Unordered with respect to the snapshot that first carries the entity it names, and
    /// deliberately so — `schemas/player.fbs` says either order is legal. The server sends
    /// it *ahead* of that snapshot where it can, which is the cheap half of making the
    /// placeholder rare rather than a guarantee the ECS may rely on.
    Appearance(PlayerAppearance),
    /// The server refused an action, with the reason a player reads.
    ///
    /// Named apart from [`Self::Refused`] below, which means there is no session at all.
    /// This one is an answer inside a session that continues.
    ActionRefused(ActionRefused),
    /// The server accepted a leave and owns this remaining duration.
    Leaving(codec::LeaveStarted),
    /// The authoritative answer to a leave-cancellation request.
    LeaveCancellation(codec::LeaveCancelResult),
    /// One accepted world-chat line, preserved in wire order for the ECS log.
    Chat(ChatMessage),
    /// One still-live party invitation, preserved in wire order for the ECS log.
    PartyInvite(PartyInvite),
    /// One complete authoritative corpse-container revision.
    LootState(codec::LootState),
    /// The authoritative end of one corpse-container view.
    LootClosed(codec::LootClosed),
    /// One authoritative monster blow that reduced this player's health.
    MobHit(MobHit),
    /// One square of the map, as the server drew it for this character.
    MapTile(codec::MapTile),
    /// One page of the additive ledger of where this character has been.
    MapExplored(codec::MapExplored),
    /// Every mark this character holds, **replacing** the client's copy wholesale.
    MarkerList(codec::MarkerList),
    /// What one visible resident is called and what they do, sent once as the entity
    /// enters view. Validated at the decode boundary; no ECS system reads it until #458,
    /// exactly as `MapTile` was carried here before the map window existed.
    ResidentAppearance(codec::ResidentAppearance),
    /// The complete price list one vendor shows this session, **replacing** the previous
    /// view of that vendor wholesale.
    VendorState(codec::VendorState),
    /// The authoritative end of one open stall.
    VendorClosed(codec::VendorClosed),
    /// One recipient's complete authoritative player-trade revision.
    PlayerTradeState(codec::PlayerTradeState),
    /// The authoritative end of one player trade.
    PlayerTradeClosed(codec::PlayerTradeClosed),
    /// Where the blizzard is in its life, with the moment the bytes arrived.
    ///
    /// The timestamp is taken on this thread for the same reason a snapshot's is: the
    /// frame that drains the channel may be delayed, and the countdown is display of the
    /// server's whole-second statement from its arrival rather than from Bevy's next
    /// opportunity to read it.
    StormWarning {
        warning: codec::StormWarning,
        at: Instant,
    },
    /// Every warded chunk column in view, **replacing** the client's previous set
    /// wholesale. Validated at the decode boundary; nothing draws it yet.
    WardsNearby(codec::WardsNearby),
    /// One Opus frame the server decided this session may hear, in wire order.
    ///
    /// Validated at the decode boundary and consumed by nothing: the audio path is #851
    /// and the settings that gate it are #852, exactly as `MapTile` crossed this channel
    /// before the map window existed. Carried rather than dropped in `codec` so that the
    /// arm which grows a decoder is the only thing left to write.
    ///
    /// **No log line may ever carry these bytes**, which is why this variant is delivered
    /// and never printed: `Warning` is the one variant on this channel that becomes text.
    VoiceHeard(codec::VoiceHeard),
    /// Something worth a line in the log happened, and the session continues.
    ///
    /// This module runs below `net/mod.rs` and so has no Bevy in scope — including
    /// `warn!`. Handing the text to the ECS keeps that boundary intact and makes
    /// the warning a *value*, which is what lets a test read one back instead of
    /// hoping a global logger was installed.
    ///
    /// Never carries a token: what is written here is written to a log.
    Warning(String),
    /// The server rejected the handshake and closed the connection. Kept as a decoded
    /// value so the ECS can distinguish the two character-name answers before rendering
    /// the same code and detail every other refusal gets.
    ServerRefused(Reject),
    /// There is no session, and this locally produced reason is what a player needs to
    /// read: an unreachable server, a TLS failure, or a peer that is not speaking this
    /// protocol.
    Refused(String),
    /// A session that existed has ended. `Some` when something went wrong.
    Ended(Option<String>),
}

/// Which transport a session is built on.
///
/// **`Encrypted` is the only variant a shipped client can name.** `Plaintext` exists
/// under `cfg(test)` and nowhere else, so "this client cannot connect in the clear" is
/// enforced by the compiler in every build a player runs rather than by a flag nobody
/// sets or a sentence nobody reads.
///
/// The seam exists because the tests below stand a stub server on a real socket and
/// drive the whole thread-and-channel boundary through it, and a rustls server needs a
/// certificate — which this repository will not carry, because a private key committed
/// as a fixture is still a private key, and cannot generate, because doing so needs a
/// fourth crate the dependency rule forbids. What those tests are about is the
/// handshake state machine, the channels and the ECS; the encryption has its own tests
/// in [`super::tls`] and on the Go side, where a certificate exists to test it with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Transport {
    #[default]
    Encrypted,
    #[cfg(test)]
    Plaintext,
}

/// One connection, whichever way it is carried.
///
/// The client's counterpart to the server's `transport.Conn`, and it earns its keep the
/// same way: everything below reads and writes without knowing which it has. Two
/// handles onto one connection is the shape a session needs — a reader parked on a poll
/// and a writer parked on a channel — and each variant provides it its own way.
pub(super) enum Wire {
    Tls(tls::TlsWire),
    #[cfg(test)]
    Plain(TcpStream),
}

impl Wire {
    fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::Tls(wire) => wire.try_clone().map(Self::Tls),
            #[cfg(test)]
            Self::Plain(socket) => socket.try_clone().map(Self::Plain),
        }
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tls(wire) => wire.set_read_timeout(timeout),
            #[cfg(test)]
            Self::Plain(socket) => socket.set_read_timeout(timeout),
        }
    }

    fn shutdown(&self) -> io::Result<()> {
        match self {
            Self::Tls(wire) => wire.shutdown(),
            #[cfg(test)]
            Self::Plain(socket) => socket.shutdown(std::net::Shutdown::Both),
        }
    }
}

impl io::Read for Wire {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tls(wire) => wire.read(out),
            #[cfg(test)]
            Self::Plain(socket) => socket.read(out),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, payload: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tls(wire) => wire.write(payload),
            #[cfg(test)]
            Self::Plain(socket) => socket.write(payload),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tls(wire) => wire.flush(),
            #[cfg(test)]
            Self::Plain(socket) => socket.flush(),
        }
    }
}

/// Which server a session is against, and everything the launch decided about it.
///
/// **The address and the expectation travel together and always have**, which is the
/// point of grouping them rather than passing five parameters: a caller cannot name one
/// without naming the other, so there is no shape of this struct in which a server is
/// dialled with nobody having said what certificate to expect there. See
/// [`tls::Expectation`] for what the two variants of that answer mean.
pub(super) struct Target {
    pub(super) addr: String,
    pub(super) expected: tls::Expectation,
    pub(super) player_name: String,
    /// `--identity`, which replaces the per-server derivation outright. Read only when
    /// `expected` is [`tls::Expectation::Listed`], because that is the only session
    /// that presents anything.
    pub(super) identity_override: Option<PathBuf>,
    /// The cached ticket this session signs in with, and `None` when this launch holds
    /// no account to present.
    ///
    /// **Unlike `identity_override` above it is not gated on `expected`, and that
    /// asymmetry is the whole of what #154 changed.** An identity token is long-lived
    /// and names a player at one server until somebody deletes it, so it is shown only
    /// to a server the list stated a certificate for. A session ticket is the other
    /// shape of credential: it names one world, it expires in hours, and `ticket.Verify`
    /// refuses it at every other world — so what an unverified address learns by being
    /// presented one is one world's session for a few hours, at an address the developer
    /// typed. That is a bounded trade, and the alternative was that development could
    /// not connect at all, because a hello presenting no ticket is `ErrTicketAbsent` and
    /// is meant to be.
    ///
    /// What is **not** widened is the certificate. [`tls::Expectation::Unlisted`] still
    /// verifies nothing and still says so, and a `Listed` mismatch is still refused
    /// inside the handshake, before this file is opened and before a byte is sent.
    pub(super) ticket: Option<PathBuf>,
    /// Where this client's per-server files live, when a caller names the directory
    /// rather than leaving it to the environment.
    ///
    /// **`None` in every shipped launch**, where the answer is the XDG data directory and
    /// nothing may choose otherwise: a client that let something else name it would be a
    /// client whose files could be put anywhere. The tests in `net/mod.rs` set it so that
    /// what a session writes lands in a scratch directory rather than in the developer's
    /// own — the same reason `identity_override` exists, one file over.
    pub(super) data_home: Option<PathBuf>,
    pub(super) transport: Transport,
}

/// Runs one connection from connect to close.
///
/// Returns when the session ends or when the ECS drops its end of the channels.
/// Every failure is reported as an event and then returned from — the thread
/// never panics, because a panicking net thread would take down a client that
/// could otherwise have shown the player what went wrong.
pub(super) fn run(
    target: Target,
    events: Sender<SessionEvent>,
    commands: Receiver<NetCommand>,
    outbound_sender: Priority,
    outbound: Receiver<Vec<u8>>,
) {
    let Target {
        addr,
        expected,
        player_name,
        identity_override,
        ticket,
        data_home,
        transport,
    } = target;

    // **Before the socket, because a session with nothing to present cannot end any
    // other way.** A server that admits players on a signed ticket refuses a hello
    // carrying none — `ErrTicketAbsent`, and correctly — so dialling first would spend
    // a TLS handshake to be told what this file already says. Read here rather than in
    // a system for the reason the identity file is: it is blocking file I/O, and this
    // thread is the only place in the client that blocks.
    let presented_ticket = match ticket {
        Some(path) => match read_cached_ticket(&path, &events) {
            Some(ticket) => Some(ticket),
            // The refusal has already been sent; nothing was dialled.
            None => return,
        },
        // No account service was named, so there is no ticket to hold and no sign-in
        // this client could have done. The hello says so — an absent ticket is what
        // `schemas/handshake.fbs` calls "no account presented" — and a server built
        // after #102 answers it with a refusal a player can read. That refusal is the
        // right one: this launch really has nothing, and the remedy is `--account-service`
        // rather than anything this client could do differently.
        None => None,
    };

    let socket = match connect(&addr) {
        Ok(socket) => socket,
        Err(err) => {
            let _ = events.send(SessionEvent::Refused(format!("cannot reach {addr}: {err}")));
            return;
        }
    };

    // Nagle would hold the handshake back waiting for a second small write that
    // never comes. Best effort: a socket that refuses the option still works.
    let _ = socket.set_nodelay(true);

    // **The identity file is opened only for a server the list named**, and that is the
    // whole of "a stored identity is never presented to an unverified server". It is not
    // a check placed before the hello — it is which variant is in hand, so there is no
    // ordering to get wrong and nothing to forget. `Unlisted` is `--server`, the
    // development path: encrypted, unverified, and therefore holding nothing to lose.
    //
    // A missing file is a first connection rather than a failure, and so is an unreadable
    // one: the server mints a fresh identity either way, and refusing over it would turn
    // a lost file into a lost game.
    let env = match data_home {
        Some(path) => Environment::rooted_at(&path),
        None => default_environment(),
    };
    let (identity, complaint) = match expected {
        tls::Expectation::Listed(_) => IdentityFile::open(&addr, identity_override, &env),
        // `Supplied` is the account service's expectation and reaches no game server:
        // `Target` is built only from a list row or from `--server`. It is matched here
        // rather than folded into a wildcard so that a fourth variant one day is a
        // compile error at this line, which is where "which credential may be presented"
        // is decided.
        tls::Expectation::Supplied(_) | tls::Expectation::Unlisted => {
            (IdentityFile::forgetful(), None)
        }
    };
    if let Some(complaint) = complaint
        && events.send(SessionEvent::Warning(complaint)).is_err()
    {
        return;
    }

    // The TLS handshake, before anything is said: there is no plaintext path, so a
    // session that cannot be encrypted is a session that does not happen. A certificate
    // that is not the one the list carried is refused here, and the message it carries is
    // the one thing on this path a player has to read rather than retry through.
    let mut stream = match transport {
        Transport::Encrypted => {
            match tls::TlsWire::connect(socket, &addr, &expected, CONNECT_TIMEOUT) {
                Ok(wire) => Wire::Tls(wire),
                Err(err) => {
                    let _ = events.send(SessionEvent::Refused(err.message()));
                    return;
                }
            }
        }
        #[cfg(test)]
        Transport::Plaintext => Wire::Plain(socket),
    };

    if let Err(err) = stream.set_read_timeout(Some(READ_TIMEOUT)) {
        let _ = events.send(SessionEvent::Refused(format!(
            "cannot configure the socket to {addr}: {err}"
        )));
        return;
    }

    // The ticket read above, which is what this server admits anybody on. It is a
    // *world* ticket — a sign-in that named the world this launch is joining — because
    // `ticket.Verify` refuses one that names another world and refuses an account
    // ticket outright. `None` is a launch with no account service at all: the hello
    // then presents no account, which `schemas/handshake.fbs` reads as a legal hello
    // and this server refuses, in as many words, with something a player can act on.
    let hello = codec::encode_client_hello(&player_name, identity.presented, presented_ticket);
    if let Err(err) = frame::write_frame(&mut stream, &hello) {
        let _ = events.send(SessionEvent::Refused(format!(
            "cannot send the handshake to {addr}: {err}"
        )));
        return;
    }

    if events.send(SessionEvent::Handshaking).is_err() {
        // The ECS is already gone; there is nobody to hand a session to.
        return;
    }

    // Which character this client played here last, read on the thread that blocks and
    // for the reason both other files are read here. It is a preselection and nothing
    // else — see [`ChosenCharacter`].
    let chosen = ChosenCharacter::open(&addr, &env);

    // **No writer thread yet, and that is what keeps one writer on this socket.** The
    // hello was written from this thread and so is the character choice that follows it;
    // the second writer starts when the welcome arrives, which is the first moment the
    // ECS has anything to send. `transport.Conn` on the server promises to survive one
    // reader and one writer and nothing more, and this is that shape held through a
    // handshake that now has a person in the middle of it.
    if let Some(ending) = pump(Connection {
        stream: &mut stream,
        addr: &addr,
        events: &events,
        commands: &commands,
        outbound,
        outbound_sender: &outbound_sender,
        identity: &identity,
        chosen: &chosen,
    }) {
        let _ = events.send(ending);
    }
}

/// The environment the default identity path is derived from.
///
/// Read once and passed as a value, so the derivation is testable without a
/// process environment to mutate — which Rust 2024 makes `unsafe`, for the good
/// reason that another thread may be reading it at the time.
#[derive(Debug, Default)]
pub(super) struct Environment {
    xdg_data_home: Option<String>,
    home: Option<String>,
}

impl Environment {
    /// What this process was started with.
    ///
    /// **Absent from a test build, and that is the whole of the fix for #230.** A test
    /// that stands up a loopback server drives the same `run` a player's launch does,
    /// and every per-server file that session writes is named from this value — so for
    /// as long as a test build could call this, `cargo test` wrote into the developer's
    /// own `$XDG_DATA_HOME`, one file per ephemeral port, forever. The `#[cfg]` is what
    /// turns "remember to inject an environment" into a compile error: under
    /// `cargo test` this function does not exist, so a call site that reaches for the
    /// real data directory cannot be written by accident and cannot be added later
    /// without the build saying so. [`default_environment`] is the fallback both call
    /// sites use, and it is where the two builds differ.
    #[cfg(not(test))]
    pub(super) fn read() -> Self {
        Self {
            xdg_data_home: std::env::var(XDG_DATA_HOME).ok(),
            home: std::env::var(HOME).ok(),
        }
    }

    /// An environment whose data directory is `path`, whatever this process was started
    /// with. See [`Target::data_home`] for who names one and why.
    pub(super) fn rooted_at(path: &Path) -> Self {
        Self {
            xdg_data_home: Some(path.to_string_lossy().into_owned()),
            home: None,
        }
    }
}

/// The environment per-server files are named from when no caller named a directory.
///
/// One function with two bodies, because the two builds want opposite things from the
/// same question. A shipped client falls back to the process environment, which is the
/// only place a player's data directory is written down. A test build falls back to
/// [`Environment::default`] — an environment naming neither `XDG_DATA_HOME` nor `HOME`,
/// which [`data_home`] answers `None` for — so a session a test forgot to give a
/// directory writes **nothing, nowhere**, rather than writing into the developer's own.
///
/// **Naming nowhere rather than a temporary directory is the deliberate half.** A
/// fallback that quietly picked somewhere writable would keep every existing test
/// passing and leave the next one just as easy to get wrong; this one is silent only for
/// a test that does not care, and a test that *does* care about the remembered character
/// already names its own root through [`Target::data_home`] and would fail loudly
/// without it. There is deliberately no cleanup anywhere in this file: nothing here
/// removes a file it did not create, least of all one under a path a developer chose.
///
/// **What `cfg(test)` does and does not cover.** It is set while this crate is compiled as
/// its own test harness — its unit tests — and not while it is compiled for something else
/// to link. An integration test under `client/tests/` would therefore link the shipped half
/// below. That is unreachable rather than merely unwritten: this package builds one target,
/// a `bin`, so there is no library for such a test to link, and `client/tests/` does not
/// exist. Both halves of that footing are asserted by
/// `scripts/test/client-data-home-isolation.test.sh`, which fails the day either changes
/// and names what has to be decided then.
#[cfg(not(test))]
pub(super) fn default_environment() -> Environment {
    Environment::read()
}

/// See the shipped half above: under `cargo test` there is no process environment to
/// fall back to, so the fallback names nowhere.
#[cfg(test)]
pub(super) fn default_environment() -> Environment {
    Environment::default()
}

/// The ticket in `path`, or `None` after a refusal has been sent explaining why there
/// is none.
///
/// **This runs on the session thread and reads the cache the sign-in wrote**, which is
/// the same fence the identity file sits behind: no ticket reaches the ECS, so there is
/// no resource for a `{:?}` to find and no name outside `net` anything could start
/// deciding from. `net/servers.rs` reads the same cache the same way on its own thread.
///
/// **An expiry that has passed is refused here rather than presented**, even though the
/// server would refuse it too. The ticket carries its own signed expiry and that is the
/// authority — this check is a courtesy, and what it buys is a sentence naming the
/// remedy instead of a handshake that ends in the server's more general refusal. The
/// clock is this machine's, so one that is wrong costs a sign-in nobody needed; the
/// opposite arrangement would cost a refusal nobody could explain.
///
/// Nothing here quotes the file's contents. It is a bearer credential.
fn read_cached_ticket(path: &Path, events: &Sender<SessionEvent>) -> Option<SessionTicket> {
    let (cached, complaint) = tickets::read(path);
    if let Some(complaint) = complaint
        && events.send(SessionEvent::Warning(complaint)).is_err()
    {
        return None;
    }

    match cached {
        Some(cached) if cached.is_live(tickets::now_unix()) => Some(cached.ticket()),
        // Reachable two ways, and one sentence covers both: a cache that held nothing
        // this client could read, and one whose ticket has run out since the launch
        // decided the login screen was not needed. Either way there is nothing to
        // present, and this server admits nobody it cannot name.
        _ => {
            let _ = events.send(SessionEvent::Refused(
                "this client holds no live sign-in for that world; sign in again.".to_owned(),
            ));
            None
        }
    }
}

/// The file this client keeps its identity for one server in, and what was in it.
///
/// `path` is `None` when no usable file could be named at all — an address that
/// does not reduce to a safe file name, or an environment with neither
/// `XDG_DATA_HOME` nor `HOME`. That is a session with no memory rather than an
/// error: the handshake still works, the server still mints an identity, and the
/// only loss is that the next connection starts a new character.
#[derive(Debug, Default)]
struct IdentityFile {
    path: Option<PathBuf>,
    /// The token to present in the hello. `None` is a first connection.
    presented: Option<PlayerToken>,
}

impl IdentityFile {
    /// Locates the file for `addr` and reads whatever is in it.
    ///
    /// `override_path` is `--identity` / `VOXELHEIM_IDENTITY`, which replaces the
    /// per-server derivation outright — the way to run two characters against one
    /// server, and the way to put the file anywhere this derivation would not.
    ///
    /// The second half of the pair is a line for the log, present exactly when
    /// something was ignored. Returned rather than logged because this module has
    /// no logger, and returned rather than swallowed because a client that
    /// silently forgets who it is every launch is a bug that looks like a feature.
    fn open(
        addr: &str,
        override_path: Option<PathBuf>,
        env: &Environment,
    ) -> (Self, Option<String>) {
        let path = match override_path {
            Some(path) => Some(path),
            None => default_identity_path(addr, env),
        };

        let Some(path) = path else {
            return (
                Self::default(),
                Some(format!(
                    "no identity file could be named for {addr}: this session will be a new \
                     character, and so will the next one. Set VOXELHEIM_IDENTITY or --identity \
                     to choose a file."
                )),
            );
        };

        match read_identity(&path) {
            Ok(presented) => (
                Self {
                    path: Some(path),
                    presented,
                },
                None,
            ),
            // Ignored, and deliberately still `path`: the welcome's token replaces
            // whatever was there, which is the only honest outcome once the bytes
            // that were in it are not a token.
            Err(Unreadable::NotAToken(complaint)) => (
                Self {
                    path: Some(path),
                    presented: None,
                },
                Some(complaint),
            ),
            // The other direction, and the difference matters: these bytes were
            // never *seen*. Overwriting them would rename a new token over a file
            // that may still hold a perfectly good identity — a permission left
            // behind by one `sudo` run, a transient I/O error — and the character it
            // names would be gone with no way back. So the path is dropped: this
            // session is a new character, the file is left exactly as it is, and a
            // player who fixes the permission gets their character back.
            Err(Unreadable::Inaccessible(complaint)) => (Self::default(), Some(complaint)),
        }
    }

    /// An identity file that will neither present anything nor keep anything.
    ///
    /// What a session against an **unlisted** server gets. Not the same as "no file
    /// could be named" even though it behaves identically: a file could be named
    /// perfectly well, and this session declines to look at it — because nothing stated
    /// which certificate to expect, so there is nobody verified to present a credential
    /// to. It is the whole of that rule, and it is a value rather than a branch: the
    /// path that cannot verify is handed an identity file with nothing in it.
    fn forgetful() -> Self {
        Self::default()
    }

    /// Whether this session has a file to compare a welcome's token against.
    ///
    /// False on two paths that behave identically and mean different things: an
    /// `Unlisted` session, which declines to look at one, and a session that could
    /// name no file at all. Either way there is nothing to compare, so the answer
    /// [`Self::store`] gives is the default rather than a finding — see
    /// [`SessionEvent::Established`] for what is reported instead.
    fn remembers(&self) -> bool {
        self.path.is_some()
    }

    /// Settles the welcome's token against the one that was presented.
    ///
    /// Answers whether this is a returning session — the only thing the client
    /// derives from a token, and it derives it for a line of status text — and
    /// stores the token **only when it differs**. A returning player therefore
    /// rewrites nothing, so an ordinary session touches the disk once, to read.
    ///
    /// A token that differs is always the one worth keeping, whatever the reason
    /// it differs: a first connection, a token this server did not recognise, or a
    /// server that re-issued. All three mean the file no longer names the identity
    /// this session has, and the server is the only source of tokens there is.
    ///
    /// The second half of the pair is a line for the log, and a failed write is
    /// only that: the session is already established, and losing the file costs
    /// the *next* launch, not this one.
    ///
    /// A `bool` rather than the `net::Identity` enum the ECS publishes, and that is
    /// the boundary rather than an oversight: `Identity` carries Bevy's `Resource`
    /// derive, and no Bevy type appears below `net/mod.rs`. The enum is made there,
    /// from this flag, which is the same trade every other value crossing this
    /// channel makes.
    #[must_use = "the second half is a failed write, and nothing else reports one"]
    fn store(&self, granted: PlayerToken) -> (bool, Option<String>) {
        if self.presented == Some(granted) {
            return (true, None);
        }

        // A session with no file to write is still a session. `open` has already
        // said so in the log; saying it again on every welcome would not help.
        let Some(path) = &self.path else {
            return (false, None);
        };
        (false, write_identity(path, granted).err())
    }
}

/// Where the identity for `addr` is kept when nothing overrides it:
/// `$XDG_DATA_HOME/voxelheim/identity/<address>`, or
/// `$HOME/.local/share/voxelheim/identity/<address>`.
///
/// One file per server address, because a token is meaningful only to the server
/// that minted it. Presenting server A's token to server B would not resume
/// anything — B never issued it — and writing B's answer over A's file would cost
/// the character on A to gain nothing on B.
pub(super) fn default_identity_path(addr: &str, env: &Environment) -> Option<PathBuf> {
    let mut path = data_home(env)?;
    path.extend(IDENTITY_DIR);
    path.push(identity_file_name(addr)?);
    Some(path)
}

/// Where the character last played on `addr` is remembered:
/// `$XDG_DATA_HOME/voxelheim/characters/<address>`.
///
/// One file per server address, for the reason the identity file has one: a character id
/// is minted per world, and the id that names Eivor on one server names somebody else's
/// character on another. The address is reduced to a file name by the same function, so
/// the two files for one server are named alike.
pub(super) fn chosen_character_path(addr: &str, env: &Environment) -> Option<PathBuf> {
    let mut path = data_home(env)?;
    path.extend(CHARACTER_DIR);
    path.push(identity_file_name(addr)?);
    Some(path)
}

/// The XDG data directory, or `None` when the environment names none.
///
/// A relative `XDG_DATA_HOME` is ignored rather than resolved, which is what the
/// XDG base directory specification says to do with one: resolving it against the
/// working directory would put the identity file wherever the client happened to
/// be launched from.
pub(super) fn data_home(env: &Environment) -> Option<PathBuf> {
    let xdg = env
        .xdg_data_home
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    if let Some(xdg) = xdg {
        return Some(xdg);
    }

    let home = env
        .home
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let mut path = PathBuf::from(home);
    path.extend(DEFAULT_DATA_HOME);
    Some(path)
}

/// Reduces a server address to a file name, or refuses.
///
/// `:` becomes `_` and the brackets of an IPv6 literal are dropped, because
/// neither survives being a path component everywhere this client runs. Every
/// other character is either already safe or a reason to answer `None`: a
/// separator, a `..`, or anything else that would make the address name a file
/// somewhere other than where it was meant to. Refusing is safe — the caller
/// treats it as "no identity file" — and inventing an escaping scheme would not
/// be, because two addresses that escaped to one name would share a character.
///
/// Letters are folded to lower case, which is the one place two spellings are
/// *meant* to land on one file. A host name is case-insensitive by the DNS
/// specification and the hex of an IPv6 literal is case-insensitive too, so
/// `MyServer:7777` and `myserver:7777` are one server — and without the fold they
/// would be two identities, costing the character of a player who typed their own
/// address with a different shift key. This is the collision the paragraph above
/// refuses only because it is not one: the two names denote the same endpoint,
/// where two escaped names denote different ones. Only ASCII is folded, and that
/// is the whole alphabet this function admits.
pub(super) fn identity_file_name(addr: &str) -> Option<String> {
    let addr = addr.trim();
    if addr.is_empty() {
        return None;
    }

    let mut name = String::with_capacity(addr.len());
    for character in addr.chars() {
        match character {
            // The brackets are punctuation around an IPv6 literal, not part of the
            // address: `[::1]:7777` and `::1` port 7777 are the same server.
            '[' | ']' => {}
            ':' => name.push('_'),
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => {
                name.push(character.to_ascii_lowercase());
            }
            _ => return None,
        }
    }

    // `.` and `..` are directories, not names, and a file name made only of dots
    // is one of those two or a near miss worth refusing anyway.
    if name.is_empty() || name.chars().all(|character| character == '.') {
        return None;
    }
    Some(name)
}

/// The character this client last played on one server, so the screen can preselect it.
///
/// **A convenience and never a claim.** What it holds is a number the *server* minted and
/// listed to this account, written down after that server welcomed a session on it. The
/// screen matches it against the list the server just sent and preselects the row it
/// finds; an id that matches nothing — a character on another account, a file from an
/// older launch, anything at all — simply preselects nothing. Nothing is decided from it
/// and nothing is sent from it.
///
/// It is deliberately **not** gated on the certificate expectation the way the identity
/// file is. The rule there is about a bearer credential — see [`Target::ticket`] for the
/// two shapes — and this is not one: the id goes nowhere on the wire except back to the
/// server that issued it, which re-reads its own store before it means anything.
///
/// **A creation writes nothing, and the reason is on the wire.** `ServerWelcome` carries
/// an `entity_id`, which names a body for one session, and no `character_id` — so a
/// client that has just created a character cannot know the id the server minted for it.
/// The next connection lists it like any other and the screen preselects the first row;
/// selecting it once is what teaches this file its id.
#[derive(Debug, Default)]
struct ChosenCharacter {
    path: Option<PathBuf>,
    /// What the file held when the session started, and what the screen preselects.
    played_before: Option<u64>,
}

impl ChosenCharacter {
    /// Reads whatever is remembered for `addr`.
    ///
    /// Every failure is "nothing is remembered": a missing file is the ordinary first
    /// visit, and one that cannot be read or does not hold a number costs a preselection
    /// and nothing else. There is deliberately no complaint for the log — unlike the
    /// identity file, where an unreadable one costs a player their character, the worst
    /// outcome here is one extra keypress.
    fn open(addr: &str, env: &Environment) -> Self {
        let Some(path) = chosen_character_path(addr, env) else {
            return Self::default();
        };
        let played_before = fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
            // Zero names no character anywhere in this contract, so a file holding one
            // is a file holding nothing.
            .filter(|id| *id != 0);
        Self {
            path: Some(path),
            played_before,
        }
    }

    /// Writes `character` down as the one this client played here, unless it already is.
    ///
    /// Answers a line for the log when the write failed, and `None` otherwise — including
    /// when there was nothing to do. A failure costs the *next* launch a preselection,
    /// which is why it is a warning rather than anything louder.
    #[must_use = "a failed write is only reported through the value this returns"]
    fn remember(&self, character: u64) -> Option<String> {
        if self.played_before == Some(character) {
            return None;
        }
        let path = self.path.as_ref()?;
        write_atomically(path, character.to_string().as_bytes())
            .err()
            .map(|err| {
                format!(
                    "cannot remember which character was played, in {}: {err}; the next launch \
                     will preselect nothing",
                    path.display()
                )
            })
    }
}

/// Why there is no token to present, when there was supposed to be one.
///
/// Two variants because the caller does two different things with them, and the
/// difference is whether the bytes were *read*. A file whose contents are known not
/// to be a token may be replaced; a file nobody could open may not, because nothing
/// knows what is in it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unreadable {
    /// The bytes were read and are not a token.
    NotAToken(String),
    /// The file could not be read at all.
    Inaccessible(String),
}

impl Unreadable {
    /// The line for the log, whichever of the two this is.
    ///
    /// Test-only: production reads the two variants apart, because the whole point
    /// of the distinction is that they are handled differently.
    #[cfg(test)]
    fn complaint(self) -> String {
        match self {
            Self::NotAToken(complaint) | Self::Inaccessible(complaint) => complaint,
        }
    }
}

/// Reads the token in `path`, if there is one to read.
///
/// Four answers, and the differences between them are the whole rule: a file that is
/// not there is a first connection (`Ok(None)`); a file holding exactly
/// [`PLAYER_TOKEN_LEN`] bytes is an identity to present; a file holding anything
/// else is [`Unreadable::NotAToken`]; and a file that will not open at all is
/// [`Unreadable::Inaccessible`]. Every one of them still yields a session.
///
/// A wrong length is not repaired, because there is nothing to repair it to — the
/// server is the only source of tokens, and it will mint a new one.
fn read_identity(path: &Path) -> Result<Option<PlayerToken>, Unreadable> {
    match fs::read(path) {
        Ok(bytes) if bytes.len() == PLAYER_TOKEN_LEN => {
            let mut token = [0u8; PLAYER_TOKEN_LEN];
            token.copy_from_slice(&bytes);
            Ok(Some(PlayerToken::from_bytes(token)))
        }
        // The length, never the bytes: this text goes to a log.
        Ok(bytes) => Err(Unreadable::NotAToken(format!(
            "the identity file {} holds {} bytes rather than {PLAYER_TOKEN_LEN}; ignoring it and \
             joining as a new character",
            path.display(),
            bytes.len()
        ))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Unreadable::Inaccessible(format!(
            "cannot read the identity file {}: {err}; joining as a new character, and leaving that \
             file untouched in case it is still someone",
            path.display()
        ))),
    }
}

/// Replaces `path` with `token`, or leaves it exactly as it was.
///
/// Temporary file, flush, rename — in that order, and the temporary file is
/// created *in the destination directory*, because a rename is only atomic within
/// one filesystem. The same shape as the server's `writeAtomic`, for the same
/// reason: a crash mid-write would otherwise leave a truncated file, and a
/// truncated identity file is a wrong-length token, which is to say a lost
/// character.
///
/// Created `0600` on Unix. The file is a bearer credential — whoever can read it
/// can be the player it names — so it is created with the mode it needs rather
/// than widened by the umask and narrowed afterwards, which would leave a window
/// where it was readable.
fn write_identity(path: &Path, token: PlayerToken) -> Result<(), String> {
    write_atomically(path, token.as_bytes())
        .map_err(|err| format!("cannot store the identity in {}: {err}", path.display()))
}

/// Replaces `path` with `bytes`, or leaves it exactly as it was.
///
/// Split out of [`write_identity`] so the cached ticket in [`super::tickets`] is written
/// the same way. Two credentials under one data directory, written the same way, is one
/// discipline; a second copy of this function would be a second discipline the first
/// time either was edited.
///
/// The mode is `0600` on Unix for both, and for the same reason: each is a bearer
/// credential — whoever can read one *is* the player or the account it names.
pub(super) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;

    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} does not name a file", path.display()),
        )
    })?;
    // Process, clock and counter. `create_new` below refuses to reuse a name, which
    // is what guarantees the mode is the one this process chose rather than one a
    // pre-planted file came with — so the name has to be unique against a *stale*
    // temporary file too, left by a crash between creating one and renaming it. The
    // pid alone is not: pids are recycled, and the counter restarts at zero with the
    // process.
    let temporary = parent.join(format!(
        ".{}.{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));

    let mut options = fs::OpenOptions::new();
    // `create_new` rather than `create`: this process is then the one that made
    // the file, so the mode below is the mode it has, and no leftover from an
    // earlier crash can be written into with permissions it chose.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let written = options
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            // The flush is what makes the rename mean something: without it the
            // directory entry can reach the disk ahead of the bytes it points at.
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, path));

    written.inspect_err(|_| {
        // A failure leaves nothing behind: the destination is untouched, and the
        // temporary file goes with the attempt that created it.
        let _ = fs::remove_file(&temporary);
    })
}

/// How many encoded voice frames may wait for the writer thread.
///
/// Eight 20 ms frames is 160 ms of speech. Deep enough to ride out a write that blocks for
/// a moment, shallow enough that a listener never hears a conversation catch up from further
/// behind than a hesitation — and small beside [`super::OUTBOUND_QUEUE`], because voice is
/// the traffic that gives way here.
// Reached through `Outbound::send_voice`, whose caller is #852 part 5.
#[allow(dead_code)]
pub(super) const VOICE_QUEUE: usize = 8;

/// How long the writer waits when both queues are empty.
///
/// **Not a poll interval for either queue** — both producers wake it — but the bound on how
/// long a writer takes to notice that the ECS has dropped its sender and the session is over.
/// A closed channel is only observable by asking, and the ordinary path for asking is a wake
/// that will now never come.
const WRITER_IDLE: Duration = Duration::from_millis(100);

/// The voice frames waiting for the writer thread.
///
/// **A second queue rather than a second producer on the first one, and the acceptance
/// criterion says why**: voice must never evict an input. `PlayerInput` and every request
/// this client originates share [`super::OUTBOUND_QUEUE`]; voice waits here, and the writer
/// empties that one before it looks at this one.
///
/// **The oldest gives way, which is the opposite of what the input channel does.** An input
/// frame describes the controls *now*, so a full queue means the newest frame supersedes what
/// is in it and dropping the newest costs nothing the next tick will not resend. Voice is a
/// sequence: no frame supersedes another, and a backlog means the socket is behind. What
/// keeps a conversation usable there is staying current — dropping the oldest costs a gap the
/// listener's packet-loss concealment fills, where dropping the newest would play the
/// conversation out at an ever-growing delay and never recover.
///
/// No Bevy type appears in this file, so the ECS side of this lives in `net/mod.rs` behind
/// [`super::Outbound`], the way every other value crossing this boundary does.
#[derive(Debug, Default)]
pub(super) struct VoiceQueue {
    shared: Mutex<Waiting>,
    /// Notified by both producers: the one that queues a voice frame, and the one that put a
    /// frame on the priority channel.
    ready: Condvar,
    /// How many frames have been dropped for a full queue since the session started. A
    /// counter for a diagnostic, never a payload — nothing here can print a voice frame,
    /// because nothing here holds one for longer than it takes to write it.
    #[allow(dead_code)]
    dropped: AtomicU64,
}

/// What the writer sleeps on, all of it behind one lock.
///
/// **The flag is not a duplicate of the channel; it is what makes the wait safe.** A condvar
/// wait is only correct when the predicate is read under the same lock the notifier takes,
/// and half of what the writer is waiting for lives in an `mpsc` channel it cannot lock. So a
/// producer that put a frame on that channel records the fact here before notifying, and the
/// writer either sees the flag and skips the wait or is already inside it and is woken.
/// Without it a frame handed over in the window between the writer's last drain and its next
/// wait would sit for [`WRITER_IDLE`] — sixty times a second, that is input arriving late.
#[derive(Debug, Default)]
struct Waiting {
    frames: VecDeque<Vec<u8>>,
    /// Set by a producer, cleared by the writer when it wakes.
    poked: bool,
}

impl VoiceQueue {
    /// Queues `frame`, dropping the oldest to make room. `true` when one was dropped.
    pub(super) fn push(&self, frame: Vec<u8>) -> bool {
        let mut waiting = held(&self.shared);
        let displaced = waiting.frames.len() >= VOICE_QUEUE;
        if displaced {
            waiting.frames.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        waiting.frames.push_back(frame);
        drop(waiting);
        self.ready.notify_all();
        displaced
    }

    /// Wakes the writer without queueing anything. Reached only through [`Priority`], which
    /// is what makes it unskippable — see that type for why it is not a method every producer
    /// is asked to remember.
    fn wake(&self) {
        held(&self.shared).poked = true;
        self.ready.notify_all();
    }

    /// How many frames a full queue has cost this session.
    pub(super) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// The oldest frame waiting, or `None`.
    fn pop(&self) -> Option<Vec<u8>> {
        held(&self.shared).frames.pop_front()
    }

    /// Waits for either producer, or for `at_most`.
    fn rest(&self, at_most: Duration) {
        let mut waiting = held(&self.shared);
        if waiting.poked || !waiting.frames.is_empty() {
            waiting.poked = false;
            return;
        }
        let (mut waiting, _) = self
            .ready
            .wait_timeout(waiting, at_most)
            .unwrap_or_else(PoisonError::into_inner);
        waiting.poked = false;
    }

    /// How many frames are waiting. Test-only: what the writer does with them is what the
    /// tests are about, and a depth nothing reads would be a number nobody could check.
    #[cfg(test)]
    pub(super) fn depth(&self) -> usize {
        held(&self.shared).frames.len()
    }
}

/// The producing end of the priority channel, with the wake the writer needs attached to it.
///
/// **A type rather than a discipline, and the review on #917 is why.** [`write_loop`] no
/// longer blocks in `Receiver::recv` — it waits on [`VoiceQueue`]'s condvar so that *both*
/// queues can wake it — so a frame handed to a bare `SyncSender` sits until the next
/// [`WRITER_IDLE`] expires. This channel has two producers: the ECS through
/// [`super::Outbound`], and this thread's own leave and leave-cancel requests. The first
/// version of this change woke the writer in one of them and not the other, which is what a
/// rule of the form "remember to call `wake` afterwards" always eventually does.
///
/// Pairing the sender with the queue removes the rule. There is no way to put a frame on this
/// channel without waking the writer, because there is no way to reach the channel except
/// through here.
#[derive(Clone, Debug)]
pub(super) struct Priority {
    frames: SyncSender<Vec<u8>>,
    voice: Arc<VoiceQueue>,
}

impl Priority {
    pub(super) fn new(frames: SyncSender<Vec<u8>>, voice: Arc<VoiceQueue>) -> Self {
        Self { frames, voice }
    }

    /// Sends, blocking while the queue is full.
    ///
    /// **The net thread only.** A Bevy system that blocked here would be a frame stalled on a
    /// network; what the ECS uses is [`Self::try_send`]. This exists for the one durable
    /// request the reader thread originates, which has to go out behind every frame already
    /// accepted rather than being dropped for a full queue.
    pub(super) fn send(&self, frame: Vec<u8>) -> Result<(), std::sync::mpsc::SendError<Vec<u8>>> {
        let sent = self.frames.send(frame);
        if sent.is_ok() {
            self.voice.wake();
        }
        sent
    }

    /// Sends without ever blocking, refusing when the queue is full.
    pub(super) fn try_send(
        &self,
        frame: Vec<u8>,
    ) -> Result<(), std::sync::mpsc::TrySendError<Vec<u8>>> {
        let sent = self.frames.try_send(frame);
        if sent.is_ok() {
            self.voice.wake();
        }
        sent
    }

    /// Queues one voice frame. `true` when the oldest was dropped to make room.
    ///
    /// It reaches the *other* queue, and it wakes the writer by itself: [`VoiceQueue::push`]
    /// notifies under the same lock the writer sleeps on, which the channel above cannot.
    pub(super) fn queue_voice(&self, frame: Vec<u8>) -> bool {
        self.voice.push(frame)
    }

    /// How many voice frames a full queue has cost this session.
    pub(super) fn voice_dropped(&self) -> u64 {
        self.voice.dropped()
    }

    /// The queue the writer thread waits on, so [`pump`] can hand it over.
    pub(super) fn voice(&self) -> &Arc<VoiceQueue> {
        &self.voice
    }

    /// How many voice frames are waiting. Test-only, for the reason [`VoiceQueue::depth`] is.
    #[cfg(test)]
    pub(super) fn voice_depth(&self) -> usize {
        self.voice.depth()
    }
}

/// A lock taken even when it is poisoned, on `audio/device.rs`'s judgement: nothing behind it
/// holds an invariant a panicking thread could have broken, and refusing would stop a session
/// sending anything for the rest of its life.
fn held<T>(what: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    what.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes whatever the ECS hands it, until there is nobody left to hand it anything.
///
/// The mirror of the server's writer goroutine, and it ends the same two ways. The channel
/// closing is the ordinary one: the ECS drops its sender when the session ends or the app
/// goes away, the channel reports itself disconnected, and this returns — dropping the last
/// handle to the socket with it.
///
/// A failed write is the other. It shuts the socket down rather than reporting the error
/// itself: that unblocks the reader, which is the thread that owns the session's story and
/// will describe the ending in the terms the player is looking at. Two threads racing to
/// explain one failure would give the ECS whichever arrived first.
///
/// **Two queues, and the order between them is the whole of what this loop decides.** The
/// priority channel is emptied first and completely; only then is one voice frame taken. So a
/// burst of voice can never delay an input frame by more than the one write already in
/// progress, and it can never occupy a slot an input needed — they are not the same slots.
/// One voice frame per pass rather than all of them, so a backlog cannot starve the check
/// above it either.
fn write_loop(mut stream: Wire, outbound: Receiver<Vec<u8>>, voice: Arc<VoiceQueue>) {
    loop {
        let mut wrote = false;
        // Everything already waiting on the priority channel, before anything else.
        loop {
            match outbound.try_recv() {
                Ok(frame) => {
                    if frame::write_frame(&mut stream, &frame).is_err() {
                        let _ = stream.shutdown();
                        return;
                    }
                    wrote = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if let Some(frame) = voice.pop() {
            if frame::write_frame(&mut stream, &frame).is_err() {
                let _ = stream.shutdown();
                return;
            }
            wrote = true;
        }

        if wrote {
            continue;
        }

        // Nothing to write. The wait is bounded rather than indefinite because a *closed*
        // channel is the one thing neither producer will ever wake this thread for: the ECS
        // dropping its sender is silent, and the drain above is what notices it on the next
        // pass.
        voice.rest(WRITER_IDLE);
    }
}

/// Everything one connection's read loop works with.
///
/// A struct rather than seven parameters, and for the reason [`Target`] is one: what a
/// reviewer has to see together is that this is one socket, its two channels, the file a
/// token may be written to and the one a character id may be written to — and that the
/// outbound channel is *held here* until the welcome rather than handed to a writer at
/// connect time.
struct Connection<'a> {
    stream: &'a mut Wire,
    addr: &'a str,
    events: &'a Sender<SessionEvent>,
    commands: &'a Receiver<NetCommand>,
    /// Handed to the writer thread when the welcome arrives, and owned here until then.
    /// See [`pump`] for why that is where it goes rather than at connect.
    outbound: Receiver<Vec<u8>>,
    /// A clone of the writer's bounded queue, kept so the reader thread can place
    /// the one durable LeaveRequest behind every frame already accepted. Blocking
    /// here is safe — this is the network thread, never a Bevy system.
    /// **A [`Priority`] and not a bare `SyncSender`**: the writer no longer blocks on this
    /// channel, so every producer on it has to wake the writer, and that is a rule a type
    /// keeps and a comment does not.
    outbound_sender: &'a Priority,
    identity: &'a IdentityFile,
    chosen: &'a ChosenCharacter,
}

/// Reads until the session ends.
///
/// Returns the event that describes the ending, or `None` when the ECS asked to
/// stop — in that case there is nobody left to tell.
///
/// **It writes as well as reads, and only until the welcome.** The character choice is
/// written from this thread, so through the whole handshake there is exactly one writer
/// on this socket; the writer thread starts on the welcome, which is the first moment the
/// ECS has anything to send. That is `transport.Conn`'s "one reader and one writer per
/// connection" held across a phase that waits for a person.
fn pump(conn: Connection<'_>) -> Option<SessionEvent> {
    let Connection {
        stream,
        addr,
        events,
        commands,
        outbound,
        outbound_sender,
        identity,
        chosen,
    } = conn;

    let mut decoder = FrameDecoder::new();
    let mut handshake = Handshake::new();
    let mut buffer = vec![0u8; READ_BUFFER_SIZE];
    // Taken by the welcome, which is where the writer thread starts.
    let mut outbound = Some(outbound);
    // Which character the choice named, kept so the welcome can write it down. `None`
    // after a creation, and that is the wire's shape rather than an omission: a welcome
    // names an entity and no character, so a client that has just made one cannot know
    // the id the server minted. See [`ChosenCharacter`].
    let mut playing: Option<u64> = None;
    let mut leave_sent = false;
    let mut leave_cancel_sent = false;

    loop {
        // Every command that has arrived rather than the first, because two can be
        // waiting: a loop that took one per read would answer the second up to
        // READ_TIMEOUT later, which on the character screen is a click that appears to
        // have done nothing.
        //
        // **In arrival order, with no priority among them.** This comment used to end
        // "stopping wins whenever it is among them", which is true only of the outcome:
        // a `Disconnect` anywhere in the queue does end the session, because the drain
        // reaches it before the next read. It is not true of the commands ahead of it. A
        // `Choose` queued first is encoded and written to the socket, and a
        // `Choice::Create` that lands there mints a character on the server — for a
        // player who has already asked to leave.
        //
        // Left as a FIFO deliberately. Draining into a buffer and answering a
        // `Disconnect` first would change what this client *sends*, and the pair only
        // arrives together in the window where the reader was blocked on READ_TIMEOUT
        // with two frames of input behind it. If that becomes worth fixing it is a
        // behaviour change with its own issue, not a comment's promise.
        loop {
            match commands.try_recv() {
                // A dropped sender means the app is shutting down, which is the same
                // instruction arriving less politely.
                Ok(NetCommand::Disconnect) | Err(TryRecvError::Disconnected) => return None,
                Ok(NetCommand::Leave) => {
                    if leave_sent || !handshake.established() {
                        continue;
                    }
                    if outbound_sender.send(codec::encode_leave_request()).is_err() {
                        return Some(SessionEvent::Ended(Some(
                            "the network writer ended before the leave request was sent".to_owned(),
                        )));
                    }
                    leave_sent = true;
                }
                Ok(NetCommand::CancelLeave) => {
                    if !leave_sent || leave_cancel_sent || !handshake.established() {
                        continue;
                    }
                    if outbound_sender
                        .send(codec::encode_leave_cancel_request())
                        .is_err()
                    {
                        return Some(SessionEvent::Ended(Some(
                            "the network writer ended before the leave cancellation was sent"
                                .to_owned(),
                        )));
                    }
                    leave_cancel_sent = true;
                }
                Ok(NetCommand::Choose(choice)) => {
                    // **The phase decides, and it decides before anything is written.**
                    // A choice arriving after `Established` would put this thread on a
                    // socket the writer thread already owns, and two writers is the one
                    // arrangement `transport.Conn` does not survive. `send_character_choice`
                    // will not send one — but that guard is a Bevy system in another
                    // module, and the invariant is this one's to keep.
                    //
                    // Moving the record ahead of the write costs nothing `Unchosen` was
                    // protecting: a write that fails ends the session on the next line,
                    // so there is no reachable state where the phase moved and the
                    // question was never asked.
                    if !handshake.chose() {
                        continue;
                    }
                    let frame = match &choice {
                        Choice::Play(character) => {
                            codec::encode_select_character_request(*character)
                        }
                        Choice::Create(request) => codec::encode_create_character_request(request),
                    };
                    if let Err(err) = frame::write_frame(stream, &frame) {
                        return Some(transport_failure(
                            &handshake,
                            addr,
                            &format!("sending the character choice failed: {err}"),
                        ));
                    }
                    playing = match choice {
                        Choice::Play(character) => Some(character),
                        Choice::Create(_) => None,
                    };
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        match stream.read(&mut buffer) {
            Ok(0) => return Some(peer_closed(&handshake, addr)),
            Ok(read) => decoder.feed(&buffer[..read]),
            // The read timeout expiring is how this loop stays responsive to
            // shutdown; it is not an error. Unix reports it as WouldBlock and
            // Windows as TimedOut.
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                return Some(transport_failure(
                    &handshake,
                    addr,
                    &format!("read failed: {err}"),
                ));
            }
        }

        loop {
            let frame = match decoder.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                // Framing we can no longer trust ends the connection: there is no
                // way to resynchronise a stream whose boundaries are unknown.
                Err(err) => return Some(protocol_failure(&handshake, addr, &err.to_string())),
            };

            let message = match codec::decode(&frame) {
                Ok(message) => message,
                Err(err) => return Some(protocol_failure(&handshake, addr, &err.to_string())),
            };

            match handshake.apply(message) {
                Ok(Transition::Characters(list)) => {
                    events
                        .send(SessionEvent::Characters {
                            list,
                            played_before: chosen.played_before,
                        })
                        .ok()?;
                }
                Ok(Transition::Established(params)) => {
                    // **The writer thread, started before the ECS is told there is a
                    // session** — which is the ordering that matters, because the frame
                    // after that event is input. `try_clone` gives it its own handle to
                    // the same socket, so this thread can block on a read while that one
                    // blocks on a channel; without a second handle one of them would have
                    // to poll.
                    //
                    // Detached rather than joined: the thread ends when the ECS drops its
                    // sender, and app teardown must not wait on a socket. See write_loop.
                    if let Some(outbound) = outbound.take() {
                        let writer = match stream.try_clone() {
                            Ok(writer) => writer,
                            Err(err) => {
                                // A session that cannot send input is not a session: a
                                // player would watch a world they could not move in. It
                                // is a refusal rather than an ending because no world has
                                // been drawn yet — the welcome has not reached the ECS.
                                return Some(SessionEvent::Refused(format!(
                                    "cannot open a writer for {addr}: {err}"
                                )));
                            }
                        };
                        let voice = Arc::clone(outbound_sender.voice());
                        if let Err(err) = thread::Builder::new()
                            .name("voxelheim-net-writer".to_owned())
                            .spawn(move || write_loop(writer, outbound, voice))
                        {
                            return Some(SessionEvent::Refused(format!(
                                "cannot start the writer thread for {addr}: {err}"
                            )));
                        }
                    }

                    let (returning, complaint) = identity.store(params.player_token);
                    if let Some(complaint) = complaint {
                        events.send(SessionEvent::Warning(complaint)).ok()?;
                    }
                    // The server took the choice, so this is the character to come back
                    // to. Written here rather than where the choice was sent, because a
                    // refused selection is not one this client played.
                    if let Some(character) = playing
                        && let Some(complaint) = chosen.remember(character)
                    {
                        events.send(SessionEvent::Warning(complaint)).ok()?;
                    }
                    // The flag only means something when there is a file behind it.
                    // See `SessionEvent::Established`.
                    let returning = identity.remembers().then_some(returning);
                    events
                        .send(SessionEvent::Established { params, returning })
                        .ok()?;
                }
                Ok(Transition::Refused(reject)) => {
                    // The code stays typed across the thread boundary. The ECS decides
                    // whether this was the answer to a creation before it turns the
                    // value into the display string `Reject::describe` owns.
                    return Some(SessionEvent::ServerRefused(reject));
                }
                Ok(Transition::World(update)) => {
                    events.send(SessionEvent::World(update)).ok()?;
                }
                Ok(Transition::Snapshot(snapshot)) => {
                    events
                        .send(SessionEvent::Snapshot {
                            snapshot,
                            at: Instant::now(),
                        })
                        .ok()?;
                }
                Ok(Transition::Inventory(inventory)) => {
                    events.send(SessionEvent::Inventory(inventory)).ok()?;
                }
                Ok(Transition::LearnedMounts(mounts)) => {
                    events.send(SessionEvent::LearnedMounts(mounts)).ok()?;
                }
                Ok(Transition::ActionRefused(refused)) => {
                    events.send(SessionEvent::ActionRefused(refused)).ok()?;
                }
                Ok(Transition::Leaving(started)) => {
                    events.send(SessionEvent::Leaving(started)).ok()?;
                }
                Ok(Transition::LeaveCancellation(result)) => {
                    if !leave_cancel_sent {
                        return Some(protocol_failure(
                            &handshake,
                            addr,
                            "LeaveCancelResult answered no cancellation request",
                        ));
                    }
                    leave_cancel_sent = false;
                    if result.accepted {
                        leave_sent = false;
                    }
                    events.send(SessionEvent::LeaveCancellation(result)).ok()?;
                }
                Ok(Transition::MineProgress(progress)) => {
                    events.send(SessionEvent::MineProgress(progress)).ok()?;
                }
                Ok(Transition::Appearance(appearance)) => {
                    events.send(SessionEvent::Appearance(appearance)).ok()?;
                }
                Ok(Transition::Chat(message)) => {
                    events.send(SessionEvent::Chat(message)).ok()?;
                }
                Ok(Transition::PartyInvite(invite)) => {
                    events.send(SessionEvent::PartyInvite(invite)).ok()?;
                }
                Ok(Transition::LootState(state)) => {
                    events.send(SessionEvent::LootState(state)).ok()?;
                }
                Ok(Transition::LootClosed(closed)) => {
                    events.send(SessionEvent::LootClosed(closed)).ok()?;
                }
                Ok(Transition::MobHit(hit)) => {
                    events.send(SessionEvent::MobHit(hit)).ok()?;
                }
                Ok(Transition::MapTile(tile)) => {
                    events.send(SessionEvent::MapTile(tile)).ok()?;
                }
                Ok(Transition::MapExplored(explored)) => {
                    events.send(SessionEvent::MapExplored(explored)).ok()?;
                }
                Ok(Transition::MarkerList(list)) => {
                    events.send(SessionEvent::MarkerList(list)).ok()?;
                }
                Ok(Transition::ResidentAppearance(resident)) => {
                    events
                        .send(SessionEvent::ResidentAppearance(resident))
                        .ok()?;
                }
                Ok(Transition::VendorState(state)) => {
                    events.send(SessionEvent::VendorState(state)).ok()?;
                }
                Ok(Transition::VendorClosed(closed)) => {
                    events.send(SessionEvent::VendorClosed(closed)).ok()?;
                }
                Ok(Transition::PlayerTradeState(state)) => {
                    events.send(SessionEvent::PlayerTradeState(state)).ok()?;
                }
                Ok(Transition::PlayerTradeClosed(closed)) => {
                    events.send(SessionEvent::PlayerTradeClosed(closed)).ok()?;
                }
                Ok(Transition::StormWarning(warning)) => {
                    events
                        .send(SessionEvent::StormWarning {
                            warning,
                            at: Instant::now(),
                        })
                        .ok()?;
                }
                Ok(Transition::WardsNearby(wards)) => {
                    events.send(SessionEvent::WardsNearby(wards)).ok()?;
                }
                Ok(Transition::VoiceHeard(heard)) => {
                    events.send(SessionEvent::VoiceHeard(heard)).ok()?;
                }
                // Deliberately silent. A server→client payload this issue does
                // not consume yet is not a problem worth a log line every tick;
                // each one becomes real in its own issue.
                Ok(Transition::Ignored(_)) => {}
                Err(err) => return Some(protocol_failure(&handshake, addr, &err.to_string())),
            }
        }
    }
}

/// The peer closed cleanly. Before the welcome that is a refusal the player has
/// to read; afterwards it is how sessions normally end.
///
/// **`Choosing` is a third answer, and finding that it was not one is what #627 came
/// down to.** The rule used to be `established()` and nothing else, so the ordinary
/// end of an unanswered character screen — `-character-timeout` expiring, which the
/// server closes on deliberately and without a reply — arrived as
/// `Refused("… closed the connection before answering the handshake")` and put the
/// client in [`ConnectionState::Rejected`]. Every word of that was wrong about this
/// case: the server *had* answered the handshake, with this account's characters,
/// which is why there was a screen up to be timed out on. A close in that phase is a
/// session ending, so it is an `Ended` and the screen says *"That session ended"*.
///
/// The other two phases keep the old answer, and mean it. `AwaitingCharacters` is a
/// server that hung up on the hello, and `AwaitingWelcome` is one that hung up on a
/// choice it never answered — in both the player is watching a status line for a
/// reason the game will not start, which is exactly what a refusal is for.
///
/// [`ConnectionState::Rejected`]: super::ConnectionState::Rejected
fn peer_closed(handshake: &Handshake, addr: &str) -> SessionEvent {
    match handshake.phase() {
        Phase::Established => SessionEvent::Ended(None),
        // The detail is a log line and nothing else — `drain_session_events` warns with
        // it and the screen shows the same sentence every ending shows. A player is not
        // told a port number to explain a timeout.
        Phase::Choosing => SessionEvent::Ended(Some(format!(
            "{addr} closed the connection while a character was being chosen"
        ))),
        Phase::AwaitingCharacters | Phase::AwaitingWelcome => SessionEvent::Refused(format!(
            "{addr} closed the connection before answering the handshake"
        )),
    }
}

/// A peer broke the contract.
///
/// Before the welcome it is a refusal, because the player is looking at a status
/// line and needs to know the game will not start and why. Afterwards it is an
/// ending: there is a world on screen, and the detail belongs in the log.
fn protocol_failure(handshake: &Handshake, addr: &str, detail: &str) -> SessionEvent {
    if handshake.established() {
        SessionEvent::Ended(Some(detail.to_owned()))
    } else {
        SessionEvent::Refused(format!(
            "{addr} is not speaking the Voxelheim protocol: {detail}"
        ))
    }
}

/// The socket failed. Same reasoning as [`protocol_failure`], different cause.
fn transport_failure(handshake: &Handshake, addr: &str, detail: &str) -> SessionEvent {
    if handshake.established() {
        SessionEvent::Ended(Some(detail.to_owned()))
    } else {
        SessionEvent::Refused(format!("{addr} {detail}"))
    }
}

/// Connects to the first address that answers, within [`CONNECT_TIMEOUT`].
///
/// `to_socket_addrs` can yield several candidates for one name (IPv6 and IPv4,
/// or a round-robin record), and `TcpStream::connect_timeout` takes exactly one —
/// so the iteration is what keeps a timeout and name resolution compatible.
fn connect(addr: &str) -> io::Result<TcpStream> {
    let mut last_error = None;

    for candidate in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&candidate, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(err) => last_error = Some(err),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "the address resolved to nothing",
        )
    }))
}

/// A directory of one test's own, removed when the test ends.
///
/// Hand-rolled because the dependency budget for this client is two crates and
/// `tempfile` is not one of them (see `client/AGENTS.md`). The name carries the
/// process id and a counter, so two tests running in parallel — which is what
/// `cargo test` does by default — never share one.
///
/// It lives here rather than inside this module's tests because `net/mod.rs`'s
/// tests need an identity file too, and two of these would be two things to keep
/// right.
#[cfg(test)]
pub(super) struct Scratch(PathBuf);

#[cfg(test)]
impl Scratch {
    pub(super) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "voxelheim-{label}-{}-{}",
            std::process::id(),
            WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("a scratch directory under the temp dir");
        Self(path)
    }

    pub(super) fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// An environment whose data directory is this scratch directory.
    ///
    /// `pub(super)` because the ticket cache in `net/tickets.rs` derives its path
    /// from the same environment this one does, and a second scratch directory
    /// would be a second thing to keep right.
    pub(super) fn environment(&self) -> Environment {
        Environment::rooted_at(&self.0)
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(byte: u8) -> PlayerToken {
        PlayerToken::from_bytes([byte; PLAYER_TOKEN_LEN])
    }

    // -------------------------------------------------------------------------
    // The second queue, and the order the writer empties the two in
    // -------------------------------------------------------------------------

    /// One frame body, distinguishable from every other by its first byte.
    fn marked(byte: u8) -> Vec<u8> {
        vec![byte; 4]
    }

    /// A writer thread on one end of a real socket pair, and the reading end.
    ///
    /// A real `TcpStream` rather than a fake: [`Wire`] is the transport enum and its plain
    /// arm exists under `cfg(test)` precisely so the thread boundary can be driven without a
    /// certificate. What is under test is the *order* frames leave in, and that is only
    /// observable at the far end of a socket.
    ///
    /// **`fill` runs before the thread is spawned, and that is what makes the order under
    /// test deterministic** — the review on #917 found the version that did not. A writer
    /// started first is woken by the first `push`, so it could drain every voice frame before
    /// the caller had queued any input, and the assertion about which came out first would
    /// pass or fail on how quickly a thread was scheduled. With both queues populated up
    /// front, the writer's first pass is the whole of what is being asserted.
    fn writer_on_a_socket(
        fill: impl FnOnce(&SyncSender<Vec<u8>>, &Arc<VoiceQueue>),
    ) -> (
        SyncSender<Vec<u8>>,
        Arc<VoiceQueue>,
        TcpStream,
        thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let writing = TcpStream::connect(addr).expect("the loopback connects");
        let (reading, _) = listener.accept().expect("the connection arrives");
        reading
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read deadline");

        let (sender, receiver) = std::sync::mpsc::sync_channel(super::super::OUTBOUND_QUEUE);
        let voice = Arc::new(VoiceQueue::default());
        fill(&sender, &voice);
        let handle = {
            let voice = Arc::clone(&voice);
            thread::spawn(move || write_loop(Wire::Plain(writing), receiver, voice))
        };
        (sender, voice, reading, handle)
    }

    /// Reads `count` framed bodies off `stream`, in the order they arrive.
    fn read_bodies(stream: &mut TcpStream, count: usize) -> Vec<Vec<u8>> {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0u8; 512];
        let mut bodies = Vec::new();
        while bodies.len() < count {
            let read = stream.read(&mut buffer).expect("the writer is still there");
            assert_ne!(
                read,
                0,
                "the writer closed with {} frames read",
                bodies.len()
            );
            decoder.feed(&buffer[..read]);
            while let Some(frame) = decoder.next_frame().expect("a frame this client wrote") {
                bodies.push(frame);
                if bodies.len() == count {
                    break;
                }
            }
        }
        bodies
    }

    /// **The acceptance criterion, at the queue.** Nine frames into a queue of eight keep the
    /// newest eight; the ninth does not fail, and the one that gives way is the oldest.
    #[test]
    fn a_full_voice_queue_keeps_the_newest_eight_and_drops_the_oldest() {
        let queue = VoiceQueue::default();
        for byte in 0..VOICE_QUEUE as u8 {
            assert!(
                !queue.push(marked(byte)),
                "frame {byte} displaced something"
            );
        }
        assert_eq!(queue.depth(), VOICE_QUEUE);
        assert_eq!(queue.dropped(), 0);

        assert!(queue.push(marked(8)), "the ninth frame displaced nothing");
        assert_eq!(queue.depth(), VOICE_QUEUE, "the queue grew past its bound");
        assert_eq!(queue.dropped(), 1);

        // What is left is frames one to eight in order: the *oldest* went, and the rest kept
        // their order rather than being reshuffled.
        let kept: Vec<u8> = (0..VOICE_QUEUE)
            .filter_map(|_| queue.pop())
            .map(|frame| frame[0])
            .collect();
        assert_eq!(kept, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(queue.pop(), None);
    }

    /// **And the other half of it: voice never touches the input queue.** Nine voice frames
    /// with nobody draining anything leave `OUTBOUND_QUEUE` completely free, so an input
    /// frame handed over afterwards is queued rather than dropped.
    #[test]
    fn a_flood_of_voice_leaves_every_input_slot_free() {
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<Vec<u8>>(super::super::OUTBOUND_QUEUE);
        let voice = Arc::new(VoiceQueue::default());
        for byte in 0..=VOICE_QUEUE as u8 {
            voice.push(marked(byte));
        }
        for byte in 0..super::super::OUTBOUND_QUEUE as u8 {
            sender
                .try_send(marked(byte))
                .unwrap_or_else(|_| panic!("voice took input slot {byte}"));
        }
        assert_eq!(voice.depth(), VOICE_QUEUE);
        drop(receiver);
    }

    /// **The writer empties the priority channel before it looks at voice**, whatever order
    /// the two were handed over in. Read at the far end of a real socket, because the order
    /// frames leave in is not observable anywhere else.
    ///
    /// Both queues are full before the writer exists, so the answer is exact rather than a
    /// band: the first pass writes all three input frames and then one voice frame, and each
    /// pass after it writes one more voice frame. Inverting the two blocks in [`write_loop`]
    /// makes this fail with `[a0, a1, a2, a3, 10, 11, 12]`.
    #[test]
    fn the_writer_drains_input_before_voice_however_they_were_queued() {
        let (sender, _voice, mut reading, handle) = writer_on_a_socket(|sender, voice| {
            // Voice first and plenty of it, then input — the order that would come out wrong
            // if the writer treated the two queues as one.
            for byte in 0..4u8 {
                voice.push(marked(0xA0 + byte));
            }
            for byte in 0..3u8 {
                sender.try_send(marked(0x10 + byte)).expect("a free slot");
            }
        });

        let bodies = read_bodies(&mut reading, 7);
        let heads: Vec<u8> = bodies.iter().map(|body| body[0]).collect();
        assert_eq!(
            heads,
            vec![0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3],
            "the writer did not empty the priority channel first"
        );

        drop(sender);
        handle.join().expect("the writer ended cleanly");
    }

    /// **Every producer on the priority channel wakes the writer, not only the ECS's.**
    ///
    /// The reader thread originates its own frames through [`Priority::send`] — the leave
    /// request and its cancellation — and the review on #917 found that half of the change
    /// woke the writer and half did not. What that cost was latency rather than a lost frame,
    /// which is exactly the kind of defect a green suite keeps: the frame does go out, a
    /// tenth of a second later. So this measures the delay.
    ///
    /// The bound is a tenth of [`WRITER_IDLE`]. A writer that was not woken cannot answer
    /// inside it; one that was answers in microseconds.
    #[test]
    fn a_frame_from_either_producer_wakes_the_writer_rather_than_waiting_out_the_timeout() {
        let (sender, voice, mut reading, handle) = writer_on_a_socket(|_, _| {});
        let priority = Priority::new(sender.clone(), Arc::clone(&voice));

        // Let the writer reach its wait before anything is sent, so what is measured is a
        // wake rather than a pass that was already running.
        thread::sleep(Duration::from_millis(20));

        for (label, byte, blocking) in [
            ("the ECS's non-blocking send", 0x21u8, false),
            ("the net thread's blocking send", 0x22u8, true),
        ] {
            let sent = if blocking {
                priority.send(marked(byte)).is_ok()
            } else {
                priority.try_send(marked(byte)).is_ok()
            };
            assert!(sent, "{label} was refused");

            let started = Instant::now();
            let body = read_bodies(&mut reading, 1);
            let waited = started.elapsed();
            assert_eq!(body[0][0], byte, "{label} produced the wrong frame");
            assert!(
                waited < WRITER_IDLE / 10,
                "{label} waited {waited:?}, which is the timeout rather than a wake"
            );
            // Back to a resting writer before the next one, so each is measured from the
            // same state.
            thread::sleep(Duration::from_millis(20));
        }

        drop(priority);
        drop(sender);
        handle.join().expect("the writer ended cleanly");
    }

    /// The ordinary ending: the ECS drops its sender and the writer returns, dropping the
    /// last handle to the socket with it. The bounded wait is what makes that observable at
    /// all — nothing wakes a writer to tell it a channel closed.
    #[test]
    fn the_writer_ends_when_the_ecs_drops_its_sender() {
        let (sender, _voice, mut reading, handle) = writer_on_a_socket(|_, voice| {
            voice.push(marked(0xA0));
        });
        assert_eq!(read_bodies(&mut reading, 1)[0][0], 0xA0);

        drop(sender);
        handle.join().expect("the writer ended cleanly");
        let mut leftover = [0u8; 8];
        assert_eq!(
            reading.read(&mut leftover).expect("the socket is readable"),
            0,
            "the writer kept its handle to the socket"
        );
    }

    // -------------------------------------------------------------------------
    // Naming the file
    // -------------------------------------------------------------------------

    #[test]
    fn an_address_becomes_a_safe_file_name() {
        for (addr, expected) in [
            ("127.0.0.1:7777", "127.0.0.1_7777"),
            ("norse.example:9000", "norse.example_9000"),
            ("localhost", "localhost"),
            // The brackets are punctuation around the literal, not address.
            ("[::1]:7777", "__1_7777"),
            ("[fe80::1]:7777", "fe80__1_7777"),
            ("host-name_2.example:1", "host-name_2.example_1"),
            ("  127.0.0.1:7777  ", "127.0.0.1_7777"),
        ] {
            assert_eq!(
                identity_file_name(addr).as_deref(),
                Some(expected),
                "{addr}"
            );
        }
    }

    #[test]
    fn an_address_that_is_not_a_safe_file_name_is_refused() {
        // Refusing costs a session's memory; escaping would cost a character, since
        // two addresses that escaped to one name would share one identity.
        for addr in [
            "",
            "   ",
            ".",
            "..",
            "...",
            "a/b:1",
            "../../etc/passwd",
            "a\\b:1",
            "évora:1",
            "star*:1",
            "with space:1",
            "new\nline:1",
        ] {
            assert_eq!(identity_file_name(addr), None, "{addr:?}");
        }
    }

    #[test]
    fn one_server_spelled_two_ways_is_one_identity() {
        // A host name is case-insensitive, so every pair below names one server. A
        // player who reaches it with a different shift key than last time must not
        // arrive as a different character.
        for (typed, expected) in [
            ("MyServer:7777", "myserver_7777"),
            ("NORSE.EXAMPLE:9000", "norse.example_9000"),
            ("LocalHost", "localhost"),
            // The hex of an IPv6 literal is case-insensitive too.
            ("[FE80::1]:7777", "fe80__1_7777"),
        ] {
            assert_eq!(
                identity_file_name(typed).as_deref(),
                Some(expected),
                "{typed}"
            );
        }

        let env = Environment {
            xdg_data_home: Some("/data".to_owned()),
            home: None,
        };
        assert_eq!(
            default_identity_path("MyServer:7777", &env),
            default_identity_path("myserver:7777", &env),
            "one server is one file however it was typed"
        );
    }

    #[test]
    fn two_servers_get_two_files_and_one_server_gets_one() {
        let env = Environment {
            xdg_data_home: Some("/data".to_owned()),
            home: None,
        };

        let a = default_identity_path("a.example:7777", &env).expect("a legal address");
        let b = default_identity_path("b.example:7777", &env).expect("a legal address");
        let again = default_identity_path("a.example:7777", &env).expect("a legal address");

        assert_ne!(a, b, "one file per server address is the whole rule");
        assert_eq!(a, again, "and the same server is the same file every time");
        assert_eq!(a, PathBuf::from("/data/voxelheim/identity/a.example_7777"));
    }

    #[test]
    fn the_data_directory_is_xdg_then_home() {
        let path = |xdg: Option<&str>, home: Option<&str>| {
            default_identity_path(
                "norse.example:9000",
                &Environment {
                    xdg_data_home: xdg.map(str::to_owned),
                    home: home.map(str::to_owned),
                },
            )
        };

        assert_eq!(
            path(Some("/data"), Some("/fixture-root")),
            Some(PathBuf::from("/data/voxelheim/identity/norse.example_9000"))
        );
        assert_eq!(
            path(None, Some("/fixture-root")),
            Some(PathBuf::from(
                "/fixture-root/.local/share/voxelheim/identity/norse.example_9000"
            ))
        );
        // An exported-but-empty variable is an unset one, here as everywhere else.
        assert_eq!(
            path(Some("  "), Some("/fixture-root")),
            Some(PathBuf::from(
                "/fixture-root/.local/share/voxelheim/identity/norse.example_9000"
            ))
        );
        // The XDG specification says to ignore a relative base directory rather
        // than resolve it: resolving would put the file wherever the client was
        // launched from.
        assert_eq!(
            path(Some("relative/data"), Some("/fixture-root")),
            Some(PathBuf::from(
                "/fixture-root/.local/share/voxelheim/identity/norse.example_9000"
            ))
        );
        // Nowhere to put it. A session with no memory, not a failure.
        assert_eq!(path(None, None), None);
        assert_eq!(path(Some("relative"), None), None);
    }

    // -------------------------------------------------------------------------
    // Reading and writing
    // -------------------------------------------------------------------------

    #[test]
    fn an_absent_file_is_a_first_connection() {
        let scratch = Scratch::new("absent");
        assert_eq!(read_identity(&scratch.join("nothing-here")), Ok(None));
    }

    #[test]
    fn a_token_round_trips_through_the_file() {
        let scratch = Scratch::new("round-trip");
        let path = scratch.join("identity");

        write_identity(&path, token(0x5a)).expect("a fresh file in a fresh directory");
        assert_eq!(read_identity(&path), Ok(Some(token(0x5a))));
        assert_eq!(
            fs::read(&path).expect("the file exists").len(),
            PLAYER_TOKEN_LEN
        );
    }

    #[test]
    fn the_directory_is_created_on_the_way() {
        // The first launch on a machine has neither the file nor the two
        // directories above it.
        let scratch = Scratch::new("mkdir");
        let path = scratch
            .join("voxelheim")
            .join("identity")
            .join("a.example_1");

        write_identity(&path, token(1)).expect("the directories are made on the way");
        assert_eq!(read_identity(&path), Ok(Some(token(1))));
    }

    #[test]
    fn a_file_of_the_wrong_length_is_ignored_with_a_complaint() {
        let scratch = Scratch::new("wrong-length");
        let path = scratch.join("identity");

        for len in [0, 7, PLAYER_TOKEN_LEN - 1, PLAYER_TOKEN_LEN + 1] {
            fs::write(&path, vec![0x5a; len]).expect("a writable scratch directory");

            let complaint = read_identity(&path)
                .expect_err("that is not a token")
                .complaint();
            assert!(complaint.contains(&len.to_string()), "{complaint}");

            // And the whole rule around it: ignored, still a first connection, and
            // the path is kept so the welcome's token replaces what is there.
            let (identity, reported) =
                IdentityFile::open("a.example:1", Some(path.clone()), &Environment::default());
            assert_eq!(identity.presented, None, "{len} bytes is not an identity");
            assert_eq!(identity.path, Some(path.clone()));
            assert_eq!(reported.as_deref(), Some(complaint.as_str()));
        }
    }

    #[test]
    fn an_unreadable_file_is_a_first_connection_rather_than_a_failure() {
        // A directory where the file should be: readable as an entry, not as bytes.
        // Losing the file must never cost the session — the server mints a fresh
        // identity, and refusing to connect would turn a lost file into a lost game.
        let scratch = Scratch::new("unreadable");
        let path = scratch.join("identity");
        fs::create_dir(&path).expect("a directory in place of the file");

        let (identity, complaint) =
            IdentityFile::open("a.example:1", Some(path.clone()), &Environment::default());

        assert_eq!(identity.presented, None);
        assert!(complaint.is_some(), "and it says so in the log");
        assert_eq!(
            identity.path, None,
            "bytes nobody could read are not bytes to write over"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_written_over() {
        // The difference between the two kinds of unreadable, and the reason they are
        // two: a wrong-length file has been *seen* not to be a token, so replacing it
        // loses nothing. A file that would not open might still hold a good identity —
        // a permission left behind by one `sudo` run, a transient I/O error — and
        // renaming a fresh token over it would end that character with no way back.
        let scratch = Scratch::new("untouchable");
        let path = scratch.join("identity");
        fs::create_dir(&path).expect("a directory in place of the file");

        let (identity, _) =
            IdentityFile::open("a.example:1", Some(path.clone()), &Environment::default());
        let (returning, complaint) = identity.store(token(7));

        assert!(!returning, "nothing was presented, so nothing came back");
        assert_eq!(complaint, None, "there was no write to fail");
        assert!(
            path.is_dir(),
            "the file that could not be read is exactly as it was"
        );
    }

    #[test]
    fn an_address_with_nowhere_to_go_is_a_session_without_memory() {
        // No override, no XDG, no HOME. The handshake still works; the only loss is
        // that the next launch starts a new character, and the log says so.
        let (identity, complaint) =
            IdentityFile::open("a.example:1", None, &Environment::default());

        assert_eq!(identity.path, None);
        assert_eq!(identity.presented, None);
        assert!(complaint.is_some(), "silently forgetting is the bug");

        // And a welcome it cannot store is still a welcome.
        let (returning, complaint) = identity.store(token(2));
        assert!(!returning);
        assert_eq!(complaint, None, "there is nothing new to say every time");
    }

    #[test]
    fn a_write_leaves_no_temporary_file_behind() {
        let scratch = Scratch::new("no-litter");
        let path = scratch.join("identity");

        write_identity(&path, token(1)).expect("a first write");
        write_identity(&path, token(2)).expect("and a replacement");

        let entries: Vec<_> = fs::read_dir(&scratch.0)
            .expect("the scratch directory")
            .map(|entry| entry.expect("a readable entry").file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "temp and rename leaves one file: {entries:?}"
        );
        assert_eq!(read_identity(&path), Ok(Some(token(2))));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_readable_by_nobody_else() {
        use std::os::unix::fs::PermissionsExt;

        // A bearer credential: whoever can read it can be the player it names. The
        // assertion is that no group or other bit is set, rather than an exact
        // 0600, because a umask can only ever take bits away from the mode the
        // file was created with.
        let scratch = Scratch::new("mode");
        let path = scratch.join("identity");
        write_identity(&path, token(3)).expect("a fresh file");

        let mode = fs::metadata(&path)
            .expect("the file exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode is {:o}", mode & 0o7777);
        assert_ne!(mode & 0o400, 0, "and the owner can still read it");
    }

    // -------------------------------------------------------------------------
    // What a welcome does to the file
    // -------------------------------------------------------------------------

    #[test]
    fn an_unchanged_token_is_not_written_back() {
        // Observed by removing the file after it has been read: a write would put
        // it back, and the point is that there is no write at all.
        let scratch = Scratch::new("unchanged");
        let path = scratch.join("identity");
        write_identity(&path, token(0x5a)).expect("a stored identity");

        let (identity, complaint) =
            IdentityFile::open("a.example:1", Some(path.clone()), &Environment::default());
        assert_eq!(identity.presented, Some(token(0x5a)));
        assert_eq!(complaint, None);

        fs::remove_file(&path).expect("the file is ours to remove");
        let (returning, complaint) = identity.store(token(0x5a));

        assert!(returning, "the server answered with what was presented");
        assert_eq!(complaint, None);
        assert!(
            !path.exists(),
            "an unchanged token must not be written back"
        );
    }

    #[test]
    fn a_token_that_differs_replaces_the_one_on_disk() {
        let scratch = Scratch::new("replaced");
        let path = scratch.join("identity");
        write_identity(&path, token(1)).expect("a stored identity");

        let (identity, _) =
            IdentityFile::open("a.example:1", Some(path.clone()), &Environment::default());
        let (returning, complaint) = identity.store(token(2));

        assert!(!returning, "a different token is a different character");
        assert_eq!(complaint, None);
        assert_eq!(read_identity(&path), Ok(Some(token(2))));
    }

    #[test]
    fn a_first_connection_stores_what_the_welcome_carried() {
        let scratch = Scratch::new("first");
        let path = scratch.join("identity");

        let (identity, complaint) =
            IdentityFile::open("a.example:1", Some(path.clone()), &Environment::default());
        assert_eq!(identity.presented, None, "nothing to present yet");
        assert_eq!(complaint, None, "and nothing to complain about");

        let (returning, complaint) = identity.store(token(9));
        assert!(!returning);
        assert_eq!(complaint, None);
        assert_eq!(read_identity(&path), Ok(Some(token(9))));
    }

    #[test]
    fn one_servers_token_never_reaches_another_servers_file() {
        // The reason the file is per-server, stated as the thing that would go wrong:
        // A's token means nothing to B, so a connection to B is a new character there
        // — and B's answer must not land in A's file, which still names a character A
        // knows.
        //
        // What this pins is the second half, because it is the half that destroys
        // something: B writes while A's file already exists beside it, and A's file
        // comes back holding A's token. The first half is structural — B reads its own
        // path, which is not A's — and `two_servers_get_two_files_and_one_server_gets_one`
        // is where that is decided.
        let scratch = Scratch::new("per-server");
        let env = scratch.environment();

        let (a, complaint) = IdentityFile::open("a.example:7777", None, &env);
        assert_eq!(complaint, None);
        assert_eq!(a.presented, None);
        let _ = a.store(token(0xaa));
        let a_path = a.path.clone().expect("A named a file");
        assert!(a_path.exists(), "A's identity is on disk before B connects");

        let (b, complaint) = IdentityFile::open("b.example:7777", None, &env);
        assert_eq!(complaint, None);
        assert_ne!(
            a.path, b.path,
            "one file per server address is the whole rule"
        );
        assert_eq!(
            b.presented, None,
            "B reads its own file, and there is nothing in it — A's token is not \
             offered to a server that never minted it"
        );
        let _ = b.store(token(0xbb));

        let b_path = b.path.expect("B named a file");
        assert_eq!(
            read_identity(&a_path),
            Ok(Some(token(0xaa))),
            "B's welcome must not have touched A's character"
        );
        assert_eq!(read_identity(&b_path), Ok(Some(token(0xbb))));
    }

    #[test]
    fn an_override_replaces_the_per_server_file_outright() {
        // How one machine runs two characters against one server.
        let scratch = Scratch::new("override");
        let chosen = scratch.join("second-character");

        let (identity, complaint) = IdentityFile::open(
            "a.example:7777",
            Some(chosen.clone()),
            &scratch.environment(),
        );

        assert_eq!(complaint, None);
        assert_eq!(identity.path, Some(chosen.clone()));
        let _ = identity.store(token(4));
        assert_eq!(read_identity(&chosen), Ok(Some(token(4))));
        assert!(
            !default_identity_path("a.example:7777", &scratch.environment())
                .expect("the derived path is still nameable")
                .exists(),
            "the per-server file was never touched"
        );
    }

    // -------------------------------------------------------------------------
    // Which character was played
    // -------------------------------------------------------------------------

    #[test]
    fn a_server_never_played_on_preselects_nothing() {
        let scratch = Scratch::new("chosen-first");
        let chosen = ChosenCharacter::open("a.example:1", &scratch.environment());

        assert_eq!(chosen.played_before, None, "an ordinary first visit");
        assert!(
            chosen.path.is_some(),
            "and one that knows where to write when a character is played"
        );
    }

    #[test]
    fn the_character_written_down_is_the_one_the_next_session_reads() {
        let scratch = Scratch::new("chosen-round-trip");
        let env = scratch.environment();

        let chosen = ChosenCharacter::open("a.example:1", &env);
        assert_eq!(chosen.remember(900), None, "a writable directory");

        assert_eq!(
            ChosenCharacter::open("a.example:1", &env).played_before,
            Some(900)
        );
        // And a second character played replaces the first, rather than adding to it.
        assert_eq!(
            ChosenCharacter::open("a.example:1", &env).remember(901),
            None
        );
        assert_eq!(
            ChosenCharacter::open("a.example:1", &env).played_before,
            Some(901)
        );
    }

    #[test]
    fn one_servers_character_never_reaches_another_servers_file() {
        // The ids are minted per world: 900 on one server names somebody else's
        // character on the next, so a shared file would preselect a stranger.
        let scratch = Scratch::new("chosen-per-server");
        let env = scratch.environment();

        assert_eq!(
            ChosenCharacter::open("a.example:1", &env).remember(900),
            None
        );

        assert_eq!(
            ChosenCharacter::open("b.example:1", &env).played_before,
            None,
            "the other server was never played on"
        );
        assert_eq!(
            ChosenCharacter::open("a.example:1", &env).played_before,
            Some(900)
        );
    }

    #[test]
    fn a_file_that_names_no_character_preselects_nothing() {
        // Every shape a file can take that is not an id, and all of them cost exactly
        // one keypress: there is no complaint, and the session runs.
        let scratch = Scratch::new("chosen-rubbish");
        let env = scratch.environment();
        let path = chosen_character_path("a.example:1", &env).expect("a path to write to");
        fs::create_dir_all(path.parent().expect("a parent directory"))
            .expect("a writable scratch directory");

        for contents in ["", "   ", "not a number", "-1", "0", "12.5", "9\n9"] {
            fs::write(&path, contents).expect("a writable file");
            assert_eq!(
                ChosenCharacter::open("a.example:1", &env).played_before,
                None,
                "{contents:?} names no character"
            );
        }

        // Trailing whitespace does not, which is what makes the file editable by hand.
        fs::write(&path, "900\n").expect("a writable file");
        assert_eq!(
            ChosenCharacter::open("a.example:1", &env).played_before,
            Some(900)
        );
    }

    #[test]
    fn the_character_already_written_down_is_not_written_again() {
        // The same character played twice is the ordinary case, and rewriting the file
        // every session would be a write per launch for a value that never changes.
        let scratch = Scratch::new("chosen-unchanged");
        let env = scratch.environment();
        let path = chosen_character_path("a.example:1", &env).expect("a path to write to");

        assert_eq!(
            ChosenCharacter::open("a.example:1", &env).remember(900),
            None
        );
        let chosen = ChosenCharacter::open("a.example:1", &env);
        fs::remove_file(&path).expect("the file this session read");

        assert_eq!(
            chosen.remember(900),
            None,
            "nothing to say and nothing to do"
        );
        assert!(
            !path.exists(),
            "the file was not rewritten, which is the whole of the check"
        );
    }

    #[test]
    fn a_character_that_cannot_be_written_down_costs_a_preselection_and_says_so() {
        // **The label carries the id on purpose**, which is what turns a one-in-thirty flake
        // into a case this test exercises every run. See the assertion below.
        let scratch = Scratch::new("chosen-blocked-900");
        let env = scratch.environment();
        let path = chosen_character_path("a.example:1", &env).expect("a path to write to");
        fs::create_dir_all(&path).expect("a directory standing where the file goes");

        let complaint = ChosenCharacter::open("a.example:1", &env)
            .remember(900)
            .expect("a write that cannot land is reported");
        let named = path.display().to_string();
        assert!(
            complaint.contains(&named),
            "the line names the file: {complaint}"
        );
        // **The path is taken out before the id is looked for, and that is not tidiness.**
        // `Scratch::new` builds its directory as `voxelheim-{label}-{pid}-{seq}`, and the
        // complaint names that path — correctly, because naming the file is the other half of
        // what this test asserts. So the id was being looked for in a string that legitimately
        // contains a process id, and a pid that *happens to contain* `900` made this line fail
        // on a message that was doing exactly what it should. It happened on `develop` at pid
        // 2900 (#430).
        //
        // **The shape of flake this was is the expensive one**: the failure names a
        // well-behaved message and reads as a real regression in whatever merged last. So the
        // label above now carries `900` too, which makes the path contain it on *every* run —
        // the case that used to be luck is the case this test is now always in, and removing
        // the path is what makes the remaining check mean what it says.
        let rest = complaint.replace(&named, "<the file>");
        assert!(
            !rest.contains("900"),
            "and not the id, which would be noise: {complaint}"
        );
    }

    #[test]
    fn an_address_with_nowhere_to_write_still_plays() {
        // No XDG, no HOME. Unlike the identity file — which costs a character when it
        // cannot be written — this one costs a keypress, so it does not even complain.
        let chosen = ChosenCharacter::open("a.example:1", &Environment::default());

        assert_eq!(chosen.path, None);
        assert_eq!(chosen.played_before, None);
        assert_eq!(
            chosen.remember(900),
            None,
            "nowhere to write is not a failure"
        );
    }

    // -------------------------------------------------------------------------
    // Secrecy
    // -------------------------------------------------------------------------

    #[test]
    fn nothing_the_identity_path_says_out_loud_carries_the_token() {
        // Every line this module can produce, collected as a log would collect
        // them, and then searched for the token in each shape it could take: raw
        // bytes, hex, and the `{:?}` of a byte array. A bearer credential in a log
        // file is the credential, and `schemas/handshake.fbs` asks for exactly
        // this: never logged, never displayed.
        let scratch = Scratch::new("secrecy");
        let path = scratch.join("identity");
        let secret = token(0x5a);
        let mut log: Vec<String> = Vec::new();

        let mut record = |complaint: Option<String>| {
            if let Some(complaint) = complaint {
                log.push(complaint);
            }
        };

        // A first connection, then a stored token, then a returning one.
        let (identity, complaint) =
            IdentityFile::open("a.example:1", Some(path.clone()), &Environment::default());
        record(complaint);
        let (_, complaint) = identity.store(secret);
        record(complaint);

        let (identity, complaint) =
            IdentityFile::open("a.example:1", Some(path.clone()), &Environment::default());
        record(complaint);
        let (returning, complaint) = identity.store(secret);
        record(complaint);
        assert!(
            returning,
            "the round trip worked, which is what is being logged"
        );

        // A file that is not a token, an unreadable one, and a write that fails.
        fs::write(&path, &secret.as_bytes()[..7]).expect("a writable directory");
        record(read_identity(&path).err().map(Unreadable::complaint));
        // A write whose directory cannot be made, and one whose destination is a
        // directory: the two ways `write_identity` fails, and both name the path.
        let wall = scratch.join("wall");
        fs::write(&wall, b"not a directory").expect("a writable scratch directory");
        record(write_identity(&wall.join("identity"), secret).err());
        let blocked = scratch.join("blocked");
        fs::create_dir(&blocked).expect("a directory in the file's place");
        record(write_identity(&blocked, secret).err());
        record(read_identity(&blocked).err().map(Unreadable::complaint));
        assert!(
            !log.is_empty(),
            "the failures above must have said something"
        );

        // And everything the ECS is told, which is what actually reaches `warn!`.
        for complaint in log.clone() {
            log.push(format!("{:?}", SessionEvent::Warning(complaint)));
        }
        log.push(format!("{secret:?}"));
        log.push(format!("{identity:?}"));

        let hex: String = secret
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let debugged = format!("{:?}", secret.as_bytes());
        for line in &log {
            assert!(
                !line
                    .as_bytes()
                    .windows(PLAYER_TOKEN_LEN)
                    .any(|window| window == secret.as_bytes()),
                "raw bytes in: {line}"
            );
            assert!(!line.contains(&hex), "hex in: {line}");
            assert!(!line.contains(&debugged), "a debugged array in: {line}");
            assert!(!line.contains("5a5a"), "any run of it in: {line}");
            assert!(!line.contains("90, 90"), "any run of it in: {line}");
        }
    }
}
