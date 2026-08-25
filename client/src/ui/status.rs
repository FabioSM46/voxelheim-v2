//! Debug text nodes: one for the connection, one for the streamed world and one for the
//! player.
//!
//! The first is the whole reason a protocol mismatch is not a panic: the rejection
//! reason the server sent is a string on screen, and the app keeps running with it
//! there. One code — `ALREADY_CONNECTED` — is turned into a sentence first, and even
//! that keeps the server's detail; see [`refusal`]. The second is how chunk streaming and meshing are observed at all —
//! loaded chunks, merged quads and the last mesh duration. The third is where the
//! *server* says the player is, which is the one number that says movement works.
//! All three lines are pure functions of resources ([`describe`], [`describe_world`],
//! [`describe_player`]), so what the player would read is testable without a window.
//!
//! **No camera is spawned here.** The player module owns the one camera, and it is a
//! `Camera3d`; `bevy_ui` renders in the 3D graph as readily as the 2D one, and two
//! cameras targeting one window would need explicit ordering and clear-colour
//! configuration to avoid one erasing the other. See the module comment in
//! `player/camera.rs`.

use std::time::Duration;

use bevy::prelude::*;

use crate::net::{
    ActionRefused, ConnectionState, Identity, RefusalInbox, RefusalReason, RefusedAction, Reject,
    ServerAddress, Session,
};
use crate::player::PlayerStats;
use crate::settings::{Corner, Settings};
use crate::world::MeshStats;

/// Distance from the top-left corner, in logical pixels.
const MARGIN: f32 = 16.0;

/// Font size for both lines, in logical pixels.
const FONT_SIZE: FontSize = FontSize::Px(18.0);

/// Vertical offset of the world line, in logical pixels. Enough to clear the
/// connection line above it at [`FONT_SIZE`].
const SECOND_LINE: f32 = MARGIN + 24.0;

/// Vertical offset of the player line, one row further down.
const THIRD_LINE: f32 = MARGIN + 48.0;

/// Vertical offset of the transient notice, one row below the player line.
const FOURTH_LINE: f32 = MARGIN + 72.0;

/// Where the readout sits when it is in the **top-left** corner: one row below the notice.
///
/// The other three corners take [`MARGIN`] flat. This one cannot: the four debug lines own
/// the top-left already, and a readout laid over them would be two sentences in one place.
/// It is still the top-left corner in the sense the setting means — it is simply the first
/// free row of it.
const READOUT_LINE: f32 = MARGIN + 96.0;

/// How much of each frame's measurement the frame-rate reading takes.
///
/// A tenth: enough that a single long frame does not make the number jump, little enough
/// that a real change is on screen within a fifth of a second. The reading is a smoothed
/// average and says so — an instantaneous frame rate is unreadable, because it changes
/// faster than an eye can follow it.
const FRAME_RATE_SMOOTHING: f32 = 0.1;

/// How long a notice stays on screen.
///
/// Long enough to read a short sentence and short enough that it is gone before the next
/// thing the player tries. Measured from the frame it was shown, and reset by the next
/// notice rather than queued behind it: the answer worth reading is the newest one.
const NOTICE_LIFETIME: Duration = Duration::from_secs(4);

/// The colour a notice is drawn in, as linear RGB.
///
/// The aiming outline's warm amber, deliberately: this line and that frame are the two
/// pieces of interface that answer the same press, and a player who has just been refused
/// should see the answer in the colour of the thing they were pointing at.
const NOTICE_COLOUR: Color = Color::linear_rgb(1.0, 0.72, 0.25);

/// Draws the connection state and the world counters, and keeps both current.
pub struct StatusUiPlugin;

impl Plugin for StatusUiPlugin {
    fn build(&self, app: &mut App) {
        // The settings own whether the readout is drawn and where. Initialised here as well
        // as by `SettingsScreenPlugin`, for the reason `Notice` is beside it: this plugin has
        // to stand on its own, and its own tests build it that way.
        app.init_resource::<Notice>()
            .init_resource::<Settings>()
            .add_systems(Startup, spawn_status_text)
            .add_systems(
                Update,
                (
                    refresh_status_text,
                    refresh_world_text,
                    refresh_player_text,
                    refresh_notice_text,
                    refresh_readout,
                ),
            );
    }
}

/// Marks the connection line, so a refresh cannot accidentally rewrite somebody
/// else's text later.
#[derive(Component)]
struct StatusText;

/// Marks the world line.
#[derive(Component)]
struct WorldText;

/// Marks the player line.
#[derive(Component)]
struct PlayerText;

/// Marks the transient notice line.
#[derive(Component)]
struct NoticeText;

/// Marks the frame-rate readout.
#[derive(Component)]
struct ReadoutText;

/// The transient sentence on screen, and when it goes away.
///
/// **It holds a sentence, never a rule.** Whatever produced it — today, one refusal from
/// the server — has already been decided somewhere else; this resource is the text and a
/// deadline, and nothing reads it back to make a decision.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Notice {
    line: String,
    /// When the line stops being shown, on `Time`'s elapsed clock. `None` when there is
    /// nothing on screen.
    until: Option<Duration>,
}

impl Notice {
    /// What is on screen right now, or the empty string.
    fn line(&self) -> &str {
        if self.until.is_some() { &self.line } else { "" }
    }

    /// Shows `line` for [`NOTICE_LIFETIME`] from `now`, replacing whatever was there.
    ///
    /// Replaces rather than queues, for the reason the inbox keeps the newest: two
    /// refusals are two different answers, and the older one is about a press the player
    /// has already stopped thinking about.
    fn show(&mut self, line: String, now: Duration) {
        self.line = line;
        self.until = Some(now + NOTICE_LIFETIME);
    }

    /// Clears the line if its time is up. Answers whether anything changed.
    fn expire(&mut self, now: Duration) -> bool {
        match self.until {
            Some(until) if now >= until => {
                self.until = None;
                self.line.clear();
                true
            }
            _ => false,
        }
    }
}

fn spawn_status_text(mut commands: Commands) {
    commands.spawn((
        StatusText,
        Text::new("Starting..."),
        TextFont {
            font_size: FONT_SIZE,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(MARGIN),
            left: Val::Px(MARGIN),
            ..default()
        },
    ));

    commands.spawn((
        WorldText,
        Text::new(NO_WORLD_YET.to_owned()),
        TextFont {
            font_size: FONT_SIZE,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(SECOND_LINE),
            left: Val::Px(MARGIN),
            ..default()
        },
    ));

    commands.spawn((
        PlayerText,
        Text::new(NO_PLAYER_YET.to_owned()),
        TextFont {
            font_size: FONT_SIZE,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(THIRD_LINE),
            left: Val::Px(MARGIN),
            ..default()
        },
    ));

    // Spawned empty and stays empty until something is refused. A node with no text
    // rather than a node that appears and disappears: the three lines above it never
    // move, and a layout that reflowed under them would make the notice the loudest thing
    // on screen instead of the briefest.
    commands.spawn((
        NoticeText,
        Text::new(String::new()),
        TextFont {
            font_size: FONT_SIZE,
            ..default()
        },
        TextColor(NOTICE_COLOUR),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(FOURTH_LINE),
            left: Val::Px(MARGIN),
            ..default()
        },
    ));

    // Spawned hidden, because the readout is off until a player switches it on. The node
    // exists either way so that switching it on is a visibility change rather than a spawn —
    // the same reasoning the notice line's empty node carries.
    commands.spawn((
        ReadoutText,
        Text::new(String::new()),
        TextFont {
            font_size: FONT_SIZE,
            ..default()
        },
        TextColor(Color::WHITE),
        Visibility::Hidden,
        corner_node(Settings::default().readout_corner()),
    ));
}

/// Where a readout sits, as a node.
///
/// A pure function of the corner, so the placement is testable without a window — which
/// matters more here than usual, since the failure it prevents is a readout drawn over the
/// four debug lines rather than beside them.
fn corner_node(corner: Corner) -> Node {
    let (top, bottom) = match corner {
        Corner::TopLeft => (Val::Px(READOUT_LINE), Val::Auto),
        Corner::TopRight => (Val::Px(MARGIN), Val::Auto),
        Corner::BottomLeft | Corner::BottomRight => (Val::Auto, Val::Px(MARGIN)),
    };
    let (left, right) = match corner {
        Corner::TopLeft | Corner::BottomLeft => (Val::Px(MARGIN), Val::Auto),
        Corner::TopRight | Corner::BottomRight => (Val::Auto, Val::Px(MARGIN)),
    };
    Node {
        position_type: PositionType::Absolute,
        top,
        bottom,
        left,
        right,
        ..default()
    }
}

/// What the readout says.
///
/// **The second number is the age of the newest snapshot, and it is deliberately not called
/// a round trip.** A round-trip time would need a message on the wire to measure it against,
/// and this issue puts the wire out of scope — so what is shown is the one thing this side
/// can observe without asking anything: how long ago the server last said where everybody
/// is. A network that has gone quiet and a server that has stopped sending look the same in
/// it, which is honest, because from here they are.
fn describe_readout(frame_rate: f32, snapshot_age: Option<Duration>) -> String {
    let age = match snapshot_age {
        Some(age) => format!("{} ms", age.as_millis()),
        None => "-".to_owned(),
    };
    format!("{frame_rate:.0} fps · snapshot {age}")
}

/// Keeps the readout current, and out of the way when it is switched off.
///
/// Both counters are measured here rather than read from a resource somebody else keeps: the
/// frame rate is this schedule's own `Time`, and the snapshot age is the moment
/// `PlayerStats.server_tick` last moved. Neither is a decision and neither is on the wire —
/// this is a line of text about the client's own clock.
fn refresh_readout(
    time: Option<Res<Time>>,
    settings: Res<Settings>,
    stats: Option<Res<PlayerStats>>,
    mut nodes: Query<(&mut Text, &mut Node, &mut Visibility), With<ReadoutText>>,
    mut smoothed: Local<f32>,
    mut newest: Local<Option<(u32, Duration)>>,
) {
    let Some(time) = time else {
        return;
    };
    let now = time.elapsed();

    let delta = time.delta_secs();
    if delta > 0.0 {
        let instant = 1.0 / delta;
        *smoothed = if *smoothed > 0.0 {
            *smoothed + (instant - *smoothed) * FRAME_RATE_SMOOTHING
        } else {
            instant
        };
    }

    // The tick, not the resource's change flag: `PlayerStats` is rewritten on frames when
    // nothing about it moved, and an age measured from that would read zero for ever.
    if let Some(tick) = stats.as_ref().and_then(|stats| stats.server_tick)
        && newest.map(|(held, _)| held) != Some(tick)
    {
        *newest = Some((tick, now));
    }
    let age = newest.map(|(_, at)| now.saturating_sub(at));

    let shown = if settings.readout_shown() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let placement = corner_node(settings.readout_corner());
    let line = describe_readout(*smoothed, age);

    for (mut text, mut node, mut visibility) in &mut nodes {
        if *visibility != shown {
            *visibility = shown;
        }
        if *node != placement {
            *node = placement.clone();
        }
        // Only while it is on screen: a hidden readout that rewrote its `String` every frame
        // would be the allocation the three lines above it are careful to avoid.
        if shown == Visibility::Visible && text.0 != line {
            text.0.clone_from(&line);
        }
    }
}

/// What the world line says before the world module has been built at all.
const NO_WORLD_YET: &str = "chunks -";

/// What the player line says before the player module has been built at all.
const NO_PLAYER_YET: &str = "player -";

fn refresh_status_text(
    state: Res<ConnectionState>,
    address: Option<Res<ServerAddress>>,
    session: Option<Res<Session>>,
    identity: Option<Res<Identity>>,
    mut nodes: Query<&mut Text, With<StatusText>>,
) {
    // Change detection rather than an unconditional rewrite: the status line
    // changes a handful of times in a session, and `Text` is a `String` that
    // would otherwise be reallocated every frame.
    let stale = state.is_changed()
        || address.as_ref().is_some_and(|address| address.is_changed())
        || session.as_ref().is_some_and(|session| session.is_changed())
        || identity
            .as_ref()
            .is_some_and(|identity| identity.is_changed());
    if !stale {
        return;
    }

    let line = describe(
        &state,
        address.as_deref().map_or("", |address| address.0.as_str()),
        session.as_deref(),
        identity.as_deref().copied(),
    );
    for mut text in &mut nodes {
        text.0.clone_from(&line);
    }
}

/// Keeps the world line current.
///
/// The counters live in a resource the world module owns, so it is optional:
/// `StatusUiPlugin` is usable — and unit-tested — without `WorldPlugin`, and the line
/// says so rather than claiming zero chunks.
///
/// One resource, deliberately, including the streamed chunk count that `ChunkStore`
/// also knows. Reading the store here would mean watching two change signals, and one
/// of them belongs to a resource the renderer takes mutably.
fn refresh_world_text(stats: Option<Res<MeshStats>>, mut nodes: Query<&mut Text, With<WorldText>>) {
    let Some(stats) = stats else {
        return;
    };

    // Change detection, same reasoning as the connection line: `MeshStats` only
    // changes when meshing does, and `Text` is a `String` that would otherwise be
    // rebuilt on every frame of a settled world.
    if !stats.is_changed() {
        return;
    }

    let line = describe_world(&stats);
    for mut text in &mut nodes {
        text.0.clone_from(&line);
    }
}

/// Keeps the player line current.
///
/// Optional for the same reason as the world line: `StatusUiPlugin` has to stand on its own,
/// and its own tests build it that way.
fn refresh_player_text(
    stats: Option<Res<PlayerStats>>,
    mut nodes: Query<&mut Text, With<PlayerText>>,
) {
    let Some(stats) = stats else {
        return;
    };
    if !stats.is_changed() {
        return;
    }

    let line = describe_player(&stats);
    for mut text in &mut nodes {
        text.0.clone_from(&line);
    }
}

/// Shows the newest refusal for a few seconds, then clears it.
///
/// **The client decides nothing here, and that is the whole design of this line.** It
/// does not evaluate whether a placement was legal, does not keep a copy of the rule that
/// produced the answer, and does not colour anything by a verdict of its own — it repeats
/// what the server said. The reason the server sends the answer at all is that it already
/// computed it and used to throw it away.
///
/// Both resources are optional so `StatusUiPlugin` still stands on its own: its tests
/// build it without `NetPlugin`, and a UI plugin that panicked without a socket would be
/// untestable exactly where it matters.
fn refresh_notice_text(
    time: Option<Res<Time>>,
    inbox: Option<ResMut<RefusalInbox>>,
    mut notice: ResMut<Notice>,
    mut nodes: Query<&mut Text, With<NoticeText>>,
) {
    let Some(time) = time else {
        return;
    };
    let now = time.elapsed();

    // The newest, not all of them: two refusals are two different answers and only one
    // line exists to show them in. Everything queued is still drained, so a burst cannot
    // accumulate into an inbox that grows for the rest of the session.
    let mut changed = notice.expire(now);
    if let Some(mut inbox) = inbox
        && let Some(refused) = inbox.take().into_iter().last()
    {
        match describe_refusal(&refused) {
            Some(line) => {
                notice.show(line, now);
                changed = true;
            }
            // A defect in this build, or a code from a contract this build has not read.
            // Neither is news the player can act on, and inventing a sentence for the
            // second would present a guess as the server's answer.
            None => warn!(
                "the server refused {:?} with {:?}, which this build has no sentence for",
                refused.action, refused.reason
            ),
        }
    }

    if !changed {
        return;
    }
    for mut text in &mut nodes {
        text.0.clear();
        text.0.push_str(notice.line());
    }
}

/// The sentence a refusal reaches the screen as, or `None` when it reaches nobody.
///
/// **Every reason is turned into prose here rather than shown as a code**, which is the
/// opposite of what `ServerReject` gets and is deliberate: a reject code is a wire-level
/// fault an operator greps for, and these are answers about the ground under a player's
/// feet. `schemas/player.fbs` spells them as domain tokens for the same reason.
///
/// Two kinds of `None`, and both are silence on purpose:
///
///   - a reason that says the *request* was wrong. A correct client never produces one,
///     so it is a defect in this build; the player did nothing and can do nothing.
///   - a reason or an action this build cannot name, which is a server one contract
///     ahead. There is no sentence to write that would not be a guess.
///
/// ASCII only, for the reason [`describe`] is: the embedded fallback font is the whole
/// font stack, and a glyph it lacks renders as nothing.
fn describe_refusal(refused: &ActionRefused) -> Option<String> {
    if refused.reason.is_client_defect() {
        return None;
    }
    let placement_reason = match refused.reason {
        RefusalReason::GroundNotGenerated | RefusalReason::SpaceNotGenerated => {
            Some("the world here has not loaded yet")
        }
        RefusalReason::GroundIsAir => Some("there is nothing solid to build on"),
        RefusalReason::SpaceBlocked => Some("something is in the way"),
        RefusalReason::OutOfReach => Some("that is too far away"),
        RefusalReason::PlayerIsDead => Some("you cannot build while dead"),
        RefusalReason::SlotEmpty | RefusalReason::SlotChanged => {
            Some("you are not holding that any more")
        }
        RefusalReason::SlotUnusable => Some("what you are holding does not build anything"),
        RefusalReason::InventoryBusy => Some("your pack was busy; try again"),
        RefusalReason::TentAlreadyPlaced => Some("you already have a tent standing"),
        RefusalReason::TooFast
        | RefusalReason::PartyFull
        | RefusalReason::NoSuchPlayer
        | RefusalReason::AlreadyInParty
        | RefusalReason::NoInvite
        | RefusalReason::NotLeader
        | RefusalReason::Unknown
        | RefusalReason::MalformedNoAnchor
        | RefusalReason::MalformedFacing
        | RefusalReason::MalformedSlot
        | RefusalReason::MalformedKind => None,
    };

    match (refused.action, refused.reason) {
        (RefusedAction::PlaceStructure, _) => {
            placement_reason.map(|reason| format!("Cannot build here: {reason}"))
        }
        (RefusedAction::Chat, RefusalReason::TooFast) => {
            Some("Cannot chat: you are sending messages too quickly".to_owned())
        }
        (RefusedAction::Party, RefusalReason::PartyFull) => {
            Some("Cannot change party: the party is full".to_owned())
        }
        (RefusedAction::Party, RefusalReason::NoSuchPlayer) => {
            Some("Cannot change party: no online player has that name".to_owned())
        }
        (RefusedAction::Party, RefusalReason::AlreadyInParty) => {
            Some("Cannot change party: that player is already in a party".to_owned())
        }
        (RefusedAction::Party, RefusalReason::NoInvite) => {
            Some("Cannot change party: there is no invitation to answer".to_owned())
        }
        (RefusedAction::Party, RefusalReason::NotLeader) => {
            Some("Cannot change party: only the party leader can do that".to_owned())
        }
        _ => None,
    }
}

/// Renders the connection state as the line a player reads.
///
/// ASCII only, deliberately: the embedded fallback font is the whole font stack
/// here, and a glyph it lacks would render as nothing — which on a status line is
/// the one failure mode that hides the message it exists to show.
/// `addr` is empty exactly while [`ConnectionState::Idle`] is the state: no server has
/// been chosen, so there is no address to name — which is the one arm below that does
/// not use it.
fn describe(
    state: &ConnectionState,
    addr: &str,
    session: Option<&Session>,
    identity: Option<Identity>,
) -> String {
    match state {
        ConnectionState::Idle => "No server chosen".to_owned(),
        ConnectionState::Connecting => format!("Connecting to {addr}..."),
        ConnectionState::Handshaking => format!("Handshaking with {addr}..."),
        // Named apart from the line above it, because they are waiting for different
        // things: a handshake is waiting for the server, and this is waiting for the
        // player. A status line that said "Handshaking" over a character screen would be
        // telling somebody their game had stalled while it held a control they had not
        // pressed yet.
        ConnectionState::Choosing => format!("Choosing a character on {addr}"),
        ConnectionState::Connected => match session {
            Some(Session(params)) => format!(
                "Connected to {addr} | {} | entity {} | seed {} | {} Hz | chunk {} | view {}",
                who(identity),
                params.entity_id,
                params.world_seed,
                params.tick_rate,
                params.chunk_size,
                params.view_distance,
            ),
            // Unreachable through the plugin, which inserts the session in the
            // same frame it reports Connected. Stated rather than unwrapped: a
            // status line is the last thing that should panic.
            None => format!("Connected to {addr}"),
        },
        ConnectionState::Leaving {
            seconds_remaining: Some(seconds_remaining),
        } => {
            format!("Leaving {addr} in {seconds_remaining}s - your character is still in the world")
        }
        ConnectionState::Leaving {
            seconds_remaining: None,
        } => format!(
            "Leaving {addr} - waiting for the server countdown; your character is still in the world"
        ),
        ConnectionState::Rejected { reason } => format!("Cannot play: {}", refusal(reason, addr)),
        ConnectionState::Disconnected => format!("Disconnected from {addr}"),
    }
}

/// Which of the two things the handshake did with the identity this client
/// presented, in the two words a player understands it in.
///
/// Nothing is decided here and nothing else on screen changes: this reports the
/// comparison the net thread made, and both answers describe a session that is
/// equally established. The resource is absent only in the frame-shaped gap the
/// `None` arm above covers, so "-" is a state nobody should see.
fn who(identity: Option<Identity>) -> &'static str {
    match identity {
        Some(Identity::Returning) => "returning",
        Some(Identity::New) => "new character",
        // A session that kept no token made no comparison, so neither of the two
        // words above is a thing this client knows. It says what it does know — an
        // account was presented and the server admitted it — and leaves which
        // character that is to the server, which is the only side that decided it.
        Some(Identity::Untold) => "signed in",
        None => "-",
    }
}

/// Turns a refusal into the sentence a player can act on.
///
/// Every reason reaches the screen as the server wrote it, with one exception:
/// `ALREADY_CONNECTED` names a situation the code alone does not explain — the same
/// identity is playing somewhere else, so the thing to do is close that session or
/// launch with a different `--identity`. The code itself is not lost, because
/// `net/mod.rs` logs the reason verbatim; this is the line on screen, not the record.
///
/// **The server's detail survives even there.** `schemas/handshake.fbs` is explicit
/// that a reject's detail is for the player to read — branch on the reason, display
/// the detail — so the sentence is added to it rather than swapped for it. If the
/// server ever says *which* session holds the identity, that reaches the screen.
///
/// The code is recovered with [`Reject::split_description`], the inverse of the
/// function `net/session.rs` used to build this string. Going through the codec is
/// what stops the two from agreeing on a separator by coincidence. A refusal that
/// was never a reject — an unreachable address, a peer that is not speaking this
/// protocol — matches no code and is shown exactly as it was written.
fn refusal(reason: &str, addr: &str) -> String {
    match Reject::split_description(reason) {
        ("ALREADY_CONNECTED", None) => format!("this identity is already connected to {addr}"),
        ("ALREADY_CONNECTED", Some(detail)) => {
            format!("this identity is already connected to {addr} ({detail})")
        }
        _ => reason.to_owned(),
    }
}

/// Renders the streaming and meshing counters.
///
/// Eight numbers, and each answers a question the others cannot. `chunks` is what the
/// server has streamed; `meshed` is how much of it is on screen, and the gap between
/// them is the meshing backlog — a gap that never closes is a stuck pipeline, while
/// a permanent small gap is just the chunks of pure air that get no mesh at all.
/// `quads` is what greedy meshing achieved: the same terrain unmerged would be
/// orders of magnitude more. `last mesh` is the mesher's own cost, excluding the
/// queue wait.
///
/// The last three are the pipeline's three backlogs, in the order work moves through
/// them: `decode` has arrived and is not voxels yet, `queued` is voxels waiting for a
/// meshing slot, `meshing` is off-thread right now. A join fills them left to right
/// and drains them the same way, so which one is standing still says which stage is
/// the bottleneck. ASCII only, for the same reason as [`describe`].
///
/// The refusal count rides inside `decode` rather than in a column of its own, because
/// it is only readable next to the depth it belongs to: a backlog holding steady at its
/// bound and a backlog that has finished draining are the same `decode` number, and the
/// count in brackets is the whole difference between them. It is a session total, so a
/// zero that stays zero is the healthy world.
fn describe_world(stats: &MeshStats) -> String {
    let last = match stats.last_mesh {
        Some(elapsed) => format!("{:.2} ms", elapsed.as_secs_f64() * 1000.0),
        // Nothing has been meshed yet — either no chunk has arrived, or the first
        // task has not come back. Not "0.00 ms", which would be a measurement.
        None => "-".to_owned(),
    };

    format!(
        "chunks {} | meshed {} | quads {} | last mesh {last} | decode {} (refused {}, \
         evicted {}) | queued {} | meshing {}",
        stats.chunks_held,
        stats.meshed_chunks,
        stats.total_quads,
        stats.decode_backlog,
        stats.decode_refused,
        stats.decode_evicted,
        stats.queued,
        stats.in_flight,
    )
}

/// Renders where the *server* says the player is.
///
/// The one line that says movement works, and every number in it is the server's. `pos` is
/// the authoritative position, interpolated for display and nothing else; `speed` is the
/// velocity the snapshot carried; `tick` is which of the server's ticks is on screen; and
/// `in view` is how many entities this session can see, itself included — the number that
/// goes up when somebody else joins. `sent`/`dropped` are the only client-side numbers, and
/// a dropped count that keeps climbing means the socket is behind. ASCII only, for the same
/// reason as [`describe`].
fn describe_player(stats: &PlayerStats) -> String {
    let Some(position) = stats.position else {
        // No snapshot has named this session's own entity yet. Not "0, 0, 0", which would be
        // a position.
        return format!(
            "player - | in view {} | sent {}",
            stats.entities, stats.inputs_sent
        );
    };

    let speed = match stats.speed {
        Some(speed) => format!("{speed:.1}"),
        None => "-".to_owned(),
    };
    let tick = match stats.server_tick {
        Some(tick) => tick.to_string(),
        None => "-".to_owned(),
    };

    format!(
        "player {:.1}, {:.1}, {:.1} | speed {speed} | tick {tick} | in view {} | sent {}, dropped {}",
        position.x, position.y, position.z, stats.entities, stats.inputs_sent, stats.inputs_dropped,
    )
}

#[cfg(test)]
mod tests {
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::RefusalInbox;
    use crate::net::SessionParams;

    fn player_stats() -> PlayerStats {
        PlayerStats {
            position: Some(Vec3::new(12.25, 64.0, -3.5)),
            speed: Some(4.3),
            entities: 2,
            server_tick: Some(1234),
            inputs_sent: 900,
            inputs_dropped: 3,
        }
    }

    /// The address a session was opened against. A `&str` rather than the resource,
    /// because `describe` takes the string: it is absent before a server is chosen, and
    /// an `Option<Res<_>>` collapses to `""` at the one call site that reads it.
    fn address() -> &'static str {
        "127.0.0.1:7777"
    }

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 3,
            spawn: [0.5, 80.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 36,
            hotbar_slots: 9,
            equipment_slots: 3,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    #[test]
    fn every_state_names_the_server() {
        for state in ConnectionState::every() {
            let line = describe(&state, address(), None, None);
            // Wildcard-free, so a state added to the enum arrives here as a build
            // failure rather than as a line nobody checked. `Choosing` was the one that
            // proved the point: it named the address from the day it was written, and
            // this sweep would not have noticed if it had not.
            let names_it = match state {
                // No server has been chosen, so there is no address to name.
                ConnectionState::Idle => false,
                // The server's own sentence, and `refusal` rewrites exactly one shape of
                // it — `ALREADY_CONNECTED`, which is meaningless without saying connected
                // to *what*. Every other reason reaches the screen as the server wrote
                // it, address or no address, which is the promise
                // `a_rejection_shows_the_servers_reason_verbatim` holds. The reason used
                // here is a bare string, so this is that case.
                ConnectionState::Rejected { .. } => false,
                ConnectionState::Connecting
                | ConnectionState::Handshaking
                | ConnectionState::Choosing
                | ConnectionState::Connected
                | ConnectionState::Leaving { .. }
                | ConnectionState::Disconnected => true,
            };
            assert_eq!(
                line.contains("127.0.0.1:7777"),
                names_it,
                "{state:?} -> {line}"
            );
        }
    }

    #[test]
    fn a_connected_line_shows_the_servers_answers() {
        let line = describe(
            &ConnectionState::Connected,
            address(),
            Some(&session()),
            Some(Identity::New),
        );

        for expected in ["entity 3", "seed 1", "20 Hz", "chunk 32", "view 8"] {
            assert!(line.contains(expected), "missing {expected:?} in {line}");
        }
    }

    #[test]
    fn a_rejection_shows_the_servers_reason_verbatim() {
        // The point of the whole status node: the reason reaches the screen
        // unedited, so a player can read it and an operator can grep for it.
        let reason = "PROTOCOL_MISMATCH: server speaks protocol 1, client speaks 2";
        let line = describe(
            &ConnectionState::Rejected {
                reason: reason.to_owned(),
            },
            address(),
            None,
            None,
        );

        assert!(line.contains(reason), "{line}");
    }

    /// The refusal string exactly as `net/session.rs` builds it — through the codec,
    /// never hand-spelled here.
    ///
    /// That is the point of the helper: this line is recognised by splitting a string
    /// somebody else joined, so a test that wrote its own `"CODE: detail"` would keep
    /// passing after the real format changed and the feature silently stopped firing.
    fn refused(code: &'static str, detail: &str) -> ConnectionState {
        ConnectionState::Rejected {
            reason: Reject {
                code,
                detail: detail.to_owned(),
            }
            .describe(),
        }
    }

    #[test]
    fn an_identity_already_playing_is_explained_rather_than_spelled() {
        // The one refusal whose code says nothing a player can act on. What they can
        // act on: close the other session, or launch with a different --identity. The
        // code itself is not lost — net/mod.rs logs the reason verbatim — but the
        // screen gets the sentence.
        let line = describe(&refused("ALREADY_CONNECTED", ""), address(), None, None);
        assert_eq!(
            line,
            "Cannot play: this identity is already connected to 127.0.0.1:7777"
        );
    }

    #[test]
    fn the_servers_detail_survives_the_sentence() {
        // `schemas/handshake.fbs`: branch on the reason, display the detail. The
        // sentence is added to what the server said, not swapped for it — so if the
        // server ever names *which* session holds the identity, a player reads it.
        let line = describe(
            &refused("ALREADY_CONNECTED", "playing from 192.0.2.5 since 19:04"),
            address(),
            None,
            None,
        );

        assert_eq!(
            line,
            "Cannot play: this identity is already connected to 127.0.0.1:7777 \
             (playing from 192.0.2.5 since 19:04)"
        );
    }

    #[test]
    fn a_reject_description_survives_the_round_trip_through_the_status_line() {
        // The join and the split are inverses, and this is what pins them together:
        // every code reaches `refusal` recognisable, whatever the separator becomes.
        for (code, detail) in [
            ("ALREADY_CONNECTED", ""),
            ("ALREADY_CONNECTED", "and it is you"),
            ("SERVER_FULL", "the realm is full"),
            ("PROTOCOL_MISMATCH", "server speaks 4, client speaks 5"),
        ] {
            let described = Reject {
                code,
                detail: detail.to_owned(),
            }
            .describe();
            let (recovered, recovered_detail) = Reject::split_description(&described);

            assert_eq!(recovered, code, "{described}");
            assert_eq!(recovered_detail.unwrap_or_default(), detail, "{described}");
        }
    }

    #[test]
    fn a_refusal_that_is_not_a_reject_code_is_untouched() {
        // An unreachable address and a peer that does not speak the protocol both
        // arrive as prose with colons in them. Nothing here may edit those.
        for reason in [
            "cannot reach 127.0.0.1:7777: connection refused",
            "127.0.0.1:7777 is not speaking the Voxelheim protocol: frame too large",
            "SERVER_FULL: the realm is full",
        ] {
            let line = describe(
                &ConnectionState::Rejected {
                    reason: reason.to_owned(),
                },
                address(),
                None,
                None,
            );

            assert_eq!(line, format!("Cannot play: {reason}"));
        }
    }

    #[test]
    fn a_connected_line_says_which_character_this_is() {
        // Derived from whether the welcome's token was the one presented, and
        // shown because a player who expected their camp and got a fresh spawn
        // deserves to know which of the two happened.
        let returning = describe(
            &ConnectionState::Connected,
            address(),
            Some(&session()),
            Some(Identity::Returning),
        );
        assert!(returning.contains("returning"), "{returning}");
        assert!(!returning.contains("new character"), "{returning}");

        let new = describe(
            &ConnectionState::Connected,
            address(),
            Some(&session()),
            Some(Identity::New),
        );
        assert!(new.contains("new character"), "{new}");

        // **And a third answer, which is neither of those two words.** A session that
        // kept no token made no comparison, so claiming either would be this client
        // answering a question only the server has the answer to.
        let untold = describe(
            &ConnectionState::Connected,
            address(),
            Some(&session()),
            Some(Identity::Untold),
        );
        assert!(untold.contains("signed in"), "{untold}");
        assert!(!untold.contains("new character"), "{untold}");
        assert!(!untold.contains("returning"), "{untold}");

        // Everything else on the line is unchanged by any of the answers.
        for expected in ["entity 3", "seed 1", "20 Hz", "chunk 32", "view 8"] {
            assert!(returning.contains(expected), "{returning}");
            assert!(new.contains(expected), "{new}");
            assert!(untold.contains(expected), "{untold}");
        }
    }

    #[test]
    fn a_status_line_never_shows_a_token() {
        // The resource the line is built from carries one, so the line is a place
        // it could reach a screen. `ANY_TOKEN` is 0x11 repeated.
        for identity in [
            None,
            Some(Identity::Returning),
            Some(Identity::New),
            Some(Identity::Untold),
        ] {
            let line = describe(
                &ConnectionState::Connected,
                address(),
                Some(&session()),
                identity,
            );
            assert!(!line.contains("11"), "{line}");
            assert!(!line.contains("17"), "{line}");
        }
    }

    /// Before a server is chosen there is no address, and the line says so rather
    /// than claiming a connection to nowhere. It is the one arm that never reads the
    /// address, which is what lets the resource be absent until a session starts.
    #[test]
    fn no_server_chosen_names_no_address() {
        let line = describe(&ConnectionState::Idle, "", None, None);
        assert_eq!(line, "No server chosen");
        // And it is the same line whatever an address would have been, because it does
        // not read one.
        assert_eq!(
            describe(&ConnectionState::Idle, address(), None, None),
            line
        );
    }

    #[test]
    fn every_line_is_ascii() {
        // The embedded fallback font is the entire font stack; a character it
        // lacks would silently blank the message.
        let lines = [
            describe(&ConnectionState::Connecting, address(), None, None),
            describe(&ConnectionState::Handshaking, address(), None, None),
            describe(
                &ConnectionState::Connected,
                address(),
                Some(&session()),
                Some(Identity::New),
            ),
            describe(
                &ConnectionState::Connected,
                address(),
                Some(&session()),
                Some(Identity::Returning),
            ),
            describe(&ConnectionState::Connected, address(), None, None),
            describe(
                &ConnectionState::Leaving {
                    seconds_remaining: Some(10),
                },
                address(),
                None,
                None,
            ),
            describe(
                &ConnectionState::Rejected {
                    reason: "SERVER_FULL".to_owned(),
                },
                address(),
                None,
                None,
            ),
            describe(
                &ConnectionState::Rejected {
                    reason: "ALREADY_CONNECTED".to_owned(),
                },
                address(),
                None,
                None,
            ),
            describe(&ConnectionState::Disconnected, address(), None, None),
            describe(&ConnectionState::Idle, "", None, None),
            describe_world(&MeshStats::default()),
            describe_world(&meshing_stats()),
            describe_player(&PlayerStats::default()),
            describe_player(&player_stats()),
            NO_WORLD_YET.to_owned(),
            NO_PLAYER_YET.to_owned(),
        ];

        for line in lines {
            assert!(line.is_ascii(), "{line}");
        }

        // Every sentence a refusal can put on screen, through the same sweep: a notice is
        // a line in the same font as the four above it, and a glyph it lacks would blank
        // exactly the message that exists to explain why nothing happened.
        for reason in EVERY_REASON {
            if let Some(line) = describe_refusal(&refusal(reason)) {
                assert!(line.is_ascii(), "{reason:?} -> {line}");
            }
        }
    }

    /// Every member of [`RefusalReason`] this build knows, `Unknown` included.
    ///
    /// Written out rather than derived, for the reason the codec's `CLASSIFICATION` table
    /// is: a list computed from the same `match` it checks would agree with every hole in
    /// that `match`. A reason appended later without a sentence fails
    /// [`every_reason_is_either_a_sentence_or_a_deliberate_silence`] only because this list
    /// has to be extended by hand to compile past it.
    const EVERY_REASON: [RefusalReason; 22] = [
        RefusalReason::Unknown,
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
        RefusalReason::MalformedNoAnchor,
        RefusalReason::MalformedFacing,
        RefusalReason::MalformedSlot,
        RefusalReason::MalformedKind,
    ];

    fn refusal(reason: RefusalReason) -> ActionRefused {
        let action = match reason {
            RefusalReason::TooFast => RefusedAction::Chat,
            RefusalReason::PartyFull
            | RefusalReason::NoSuchPlayer
            | RefusalReason::AlreadyInParty
            | RefusalReason::NoInvite
            | RefusalReason::NotLeader => RefusedAction::Party,
            _ => RefusedAction::PlaceStructure,
        };
        ActionRefused {
            action,
            reason,
            anchor: Some(crate::net::BlockCoord { x: 0, y: 63, z: 0 }),
        }
    }

    /// Every reason either becomes a sentence or is deliberately shown to nobody.
    ///
    /// The two silences are not the same thing and both are on purpose:
    ///
    ///   - a reason that says the *request* was wrong. A correct client never produces one,
    ///     so it is this build's own defect; the player did nothing and can do nothing.
    ///   - `Unknown`, which is a server one contract ahead. Writing a sentence for a code
    ///     this build cannot read would present a guess as the server's answer.
    ///
    /// A reason appended later lands in neither, so it has to be given one or the other
    /// here — which is the whole point of sweeping the list rather than spot-checking it.
    #[test]
    fn every_reason_is_either_a_sentence_or_a_deliberate_silence() {
        for reason in EVERY_REASON {
            let shown = describe_refusal(&refusal(reason));
            let silent = reason == RefusalReason::Unknown || reason.is_client_defect();
            assert_eq!(
                shown.is_none(),
                silent,
                "{reason:?} -> {shown:?}; silence is for this build's own defects and for \
                 codes it cannot read, and for nothing else"
            );
            if let Some(line) = shown {
                assert!(line.starts_with("Cannot "), "{reason:?} -> {line}");
            }
        }
    }

    #[test]
    fn chat_and_party_refusals_have_specific_sentences() {
        for (reason, want) in [
            (
                RefusalReason::TooFast,
                "Cannot chat: you are sending messages too quickly",
            ),
            (
                RefusalReason::PartyFull,
                "Cannot change party: the party is full",
            ),
            (
                RefusalReason::NoSuchPlayer,
                "Cannot change party: no online player has that name",
            ),
            (
                RefusalReason::AlreadyInParty,
                "Cannot change party: that player is already in a party",
            ),
            (
                RefusalReason::NoInvite,
                "Cannot change party: there is no invitation to answer",
            ),
            (
                RefusalReason::NotLeader,
                "Cannot change party: only the party leader can do that",
            ),
        ] {
            assert_eq!(describe_refusal(&refusal(reason)).as_deref(), Some(want));
        }
    }

    /// An action this build has no verb for says nothing, whatever the reason is.
    ///
    /// Placement is the only action a server fills this message in for today; the rest of
    /// `RefusedAction` is reserved by the contract and becomes real in its own issue. A
    /// mining refusal shown with the placement's sentence would be worse than silence: it
    /// would be the wrong answer, confidently.
    #[test]
    fn a_refusal_for_an_action_this_build_has_no_verb_for_says_nothing() {
        for action in [
            RefusedAction::Unknown,
            RefusedAction::MineBlock,
            RefusedAction::EditBlock,
            RefusedAction::Craft,
            RefusedAction::Repair,
        ] {
            let refused = ActionRefused {
                action,
                reason: RefusalReason::GroundIsAir,
                anchor: None,
            };
            assert_eq!(describe_refusal(&refused), None, "{action:?}");
        }
    }

    fn meshing_stats() -> MeshStats {
        MeshStats {
            chunks_held: 217,
            meshed_chunks: 96,
            total_quads: 48_213,
            last_mesh: Some(Duration::from_micros(3_412)),
            in_flight: 4,
            queued: 12,
            decode_backlog: 37,
            decode_refused: 512,
            decode_evicted: 6,
        }
    }

    #[test]
    fn the_world_line_reports_every_counter() {
        let line = describe_world(&meshing_stats());

        for expected in [
            "chunks 217",
            "meshed 96",
            "quads 48213",
            "last mesh 3.41 ms",
            "decode 37 (refused 512, evicted 6)",
            "queued 12",
            "meshing 4",
        ] {
            assert!(line.contains(expected), "missing {expected:?} in {line}");
        }
    }

    #[test]
    fn an_unmeasured_mesh_duration_is_not_reported_as_zero() {
        // "0.00 ms" would be a measurement. Before the first task comes back there
        // is no measurement to report.
        let line = describe_world(&MeshStats {
            chunks_held: 3,
            ..MeshStats::default()
        });

        assert!(line.contains("last mesh -"), "{line}");
        assert!(!line.contains("0.00 ms"), "{line}");
    }

    #[test]
    fn the_world_line_follows_the_counters() {
        let mut app = headless_ui(ConnectionState::Connected);
        app.insert_resource(MeshStats::default());
        app.update();
        assert_eq!(world_line(&mut app), describe_world(&MeshStats::default()));

        *app.world_mut().resource_mut::<MeshStats>() = meshing_stats();
        app.update();

        let line = world_line(&mut app);
        assert!(line.contains("quads 48213"), "{line}");
        assert!(line.contains("last mesh 3.41 ms"), "{line}");
    }

    #[test]
    fn the_world_line_says_nothing_rather_than_zero_without_the_world_plugin() {
        // `StatusUiPlugin` has to stand on its own — its own tests build it that way.
        // Reporting "chunks 0" with no world module would be a claim, not a reading.
        let mut app = headless_ui(ConnectionState::Connecting);
        app.update();

        assert_eq!(world_line(&mut app), NO_WORLD_YET);
    }

    #[test]
    fn the_player_line_reports_the_servers_answers() {
        let line = describe_player(&player_stats());

        for expected in [
            "player 12.2, 64.0, -3.5",
            "speed 4.3",
            "tick 1234",
            "in view 2",
            "sent 900",
            "dropped 3",
        ] {
            assert!(line.contains(expected), "missing {expected:?} in {line}");
        }
    }

    #[test]
    fn an_unknown_position_is_not_reported_as_the_origin() {
        // "0.0, 0.0, 0.0" would be a claim about where the player is. Before the first
        // snapshot names this session's entity there is no such claim to make.
        let line = describe_player(&PlayerStats {
            entities: 0,
            ..PlayerStats::default()
        });

        assert!(line.contains("player -"), "{line}");
        assert!(!line.contains("0.0, 0.0, 0.0"), "{line}");
    }

    #[test]
    fn the_player_line_follows_the_stats() {
        let mut app = headless_ui(ConnectionState::Connected);
        app.insert_resource(PlayerStats::default());
        app.update();
        assert_eq!(
            player_line(&mut app),
            describe_player(&PlayerStats::default())
        );

        *app.world_mut().resource_mut::<PlayerStats>() = player_stats();
        app.update();

        let line = player_line(&mut app);
        assert!(line.contains("player 12.2, 64.0, -3.5"), "{line}");
        assert!(line.contains("tick 1234"), "{line}");
    }

    #[test]
    fn the_player_line_says_nothing_rather_than_zero_without_the_player_plugin() {
        // `StatusUiPlugin` has to stand on its own — its own tests build it that way.
        let mut app = headless_ui(ConnectionState::Connecting);
        app.update();

        assert_eq!(player_line(&mut app), NO_PLAYER_YET);
    }

    /// The text actually on the player node.
    fn player_line(app: &mut App) -> String {
        let world = app.world_mut();
        let mut nodes = world.query_filtered::<&Text, With<PlayerText>>();
        let lines: Vec<String> = nodes.iter(world).map(|text| text.0.clone()).collect();

        assert_eq!(lines.len(), 1, "exactly one player node exists");
        lines.into_iter().next().expect("just counted one")
    }

    /// Builds the UI headlessly. `MinimalPlugins` has no renderer, so the node is
    /// spawned and updated but never drawn — which is exactly the part worth
    /// asserting, and it needs no display.
    fn headless_ui(state: ConnectionState) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ServerAddress(address().to_owned()))
            .insert_resource(state)
            .add_plugins(StatusUiPlugin);
        app
    }

    /// The text actually on the status node.
    fn status_line(app: &mut App) -> String {
        let world = app.world_mut();
        let mut nodes = world.query_filtered::<&Text, With<StatusText>>();
        let lines: Vec<String> = nodes.iter(world).map(|text| text.0.clone()).collect();

        assert_eq!(lines.len(), 1, "exactly one status node exists");
        lines.into_iter().next().expect("just counted one")
    }

    /// The text actually on the world node.
    fn world_line(app: &mut App) -> String {
        let world = app.world_mut();
        let mut nodes = world.query_filtered::<&Text, With<WorldText>>();
        let lines: Vec<String> = nodes.iter(world).map(|text| text.0.clone()).collect();

        assert_eq!(lines.len(), 1, "exactly one world node exists");
        lines.into_iter().next().expect("just counted one")
    }

    #[test]
    fn the_plugin_spawns_one_status_node() {
        let mut app = headless_ui(ConnectionState::Connecting);
        app.update();

        assert!(status_line(&mut app).contains("Connecting to 127.0.0.1:7777"));
    }

    #[test]
    fn the_node_shows_a_rejection_reason_rather_than_the_app_dying() {
        // The acceptance criterion, end to end on the UI side: a reason placed in
        // the resource reaches the text node, and the app is still updating after
        // it did.
        let reason = "PROTOCOL_MISMATCH: server speaks protocol 1, client speaks 2";
        let mut app = headless_ui(ConnectionState::Connecting);
        app.update();

        *app.world_mut().resource_mut::<ConnectionState>() = ConnectionState::Rejected {
            reason: reason.to_owned(),
        };
        app.update();
        assert!(status_line(&mut app).contains(reason));

        // Still alive, and still saying the same thing several frames later.
        for _ in 0..8 {
            app.update();
        }
        assert!(status_line(&mut app).contains(reason));
    }

    #[test]
    fn the_node_follows_the_session_once_it_exists() {
        let mut app = headless_ui(ConnectionState::Connecting);
        app.update();

        app.world_mut().insert_resource(session());
        *app.world_mut().resource_mut::<ConnectionState>() = ConnectionState::Connected;
        app.update();

        let line = status_line(&mut app);
        for expected in ["entity 3", "20 Hz", "chunk 32", "view 8"] {
            assert!(line.contains(expected), "missing {expected:?} in {line}");
        }
    }

    /// The transient line, read off the node the player would be looking at.
    fn notice_line(app: &mut App) -> String {
        let world = app.world_mut();
        let mut nodes = world.query_filtered::<&Text, With<NoticeText>>();
        let lines: Vec<String> = nodes.iter(world).map(|text| text.0.clone()).collect();

        assert_eq!(lines.len(), 1, "exactly one notice node exists");
        lines.into_iter().next().expect("just counted one")
    }

    /// The step these tests advance the clock by, one per `update`.
    ///
    /// Small deliberately. `Time<Virtual>` clamps a frame's delta at 0.25s so a stalled
    /// frame cannot teleport a simulation, and the notice reads that clock like every
    /// other timed thing in this crate — so a test that asked for four seconds in one
    /// update would get a quarter of one, and a timeout test that passed anyway would be
    /// passing for the wrong reason.
    const STEP: Duration = Duration::from_millis(100);

    /// A status UI with a network inbox, on the fixed clock above.
    fn headless_notice_ui() -> App {
        let mut app = headless_ui(ConnectionState::Connected);
        app.init_resource::<RefusalInbox>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        app
    }

    /// Runs `count` frames of [`STEP`] each.
    fn steps(app: &mut App, count: u32) {
        for _ in 0..count {
            app.update();
        }
    }

    /// Enough frames for [`NOTICE_LIFETIME`] to have passed, with one to spare.
    fn frames_past_the_lifetime() -> u32 {
        (NOTICE_LIFETIME.as_millis() / STEP.as_millis()) as u32 + 2
    }

    /// A refusal from the server reaches the screen, and leaves it on its own.
    ///
    /// The whole point of the message: a placement that does not happen is an answer the
    /// player can read, where before it was a click that vanished. Nothing here is
    /// decided on this side — the reason arrived, and this is the sentence for it.
    #[test]
    fn a_refusal_reaches_the_screen_and_then_goes_away() {
        let mut app = headless_notice_ui();
        steps(&mut app, 1);
        assert_eq!(notice_line(&mut app), "", "nothing has been refused yet");

        app.world_mut()
            .resource_mut::<RefusalInbox>()
            .push(refusal(RefusalReason::GroundIsAir));
        steps(&mut app, 1);

        assert_eq!(
            notice_line(&mut app),
            "Cannot build here: there is nothing solid to build on"
        );
        assert_eq!(
            app.world().resource::<RefusalInbox>().pending(),
            0,
            "the inbox was drained rather than left to grow"
        );

        // Still there halfway through, and gone once its time is up. Both halves matter:
        // a line that vanished on the next frame would be a line nobody could read, and
        // one that never left would be the last refusal sitting under a placement that
        // has since worked.
        steps(&mut app, frames_past_the_lifetime() / 2);
        assert!(!notice_line(&mut app).is_empty(), "still readable");

        steps(&mut app, frames_past_the_lifetime());
        assert_eq!(notice_line(&mut app), "", "the notice expired");
    }

    /// The newest refusal wins, rather than the oldest holding the line.
    ///
    /// Two refusals are two different answers and there is one line to show them in. The
    /// one worth reading is the one about the press the player just made.
    #[test]
    fn the_newest_refusal_is_the_one_on_screen() {
        let mut app = headless_notice_ui();
        steps(&mut app, 1);

        {
            let mut inbox = app.world_mut().resource_mut::<RefusalInbox>();
            inbox.push(refusal(RefusalReason::GroundIsAir));
            inbox.push(refusal(RefusalReason::TentAlreadyPlaced));
        }
        steps(&mut app, 1);

        assert_eq!(
            notice_line(&mut app),
            "Cannot build here: you already have a tent standing"
        );
    }

    /// A refusal that names this build's own defect reaches the log, never the player.
    ///
    /// The split `schemas/player.fbs` draws between its two reason groups, doing the one
    /// thing it exists to do. A player who sent a malformed frame did nothing and can do
    /// nothing about it; telling them would be noise about somebody else's bug.
    #[test]
    fn a_refusal_about_this_builds_own_defect_never_reaches_the_player() {
        let mut app = headless_notice_ui();
        steps(&mut app, 1);

        app.world_mut()
            .resource_mut::<RefusalInbox>()
            .push(refusal(RefusalReason::MalformedFacing));
        steps(&mut app, 1);

        assert_eq!(notice_line(&mut app), "");
        assert_eq!(
            app.world().resource::<RefusalInbox>().pending(),
            0,
            "drained even though nothing was shown"
        );
    }

    /// The status plugin still stands on its own without a network.
    ///
    /// `StatusUiPlugin` is built here with no `NetPlugin`, so the inbox does not exist —
    /// which is every frame of its own tests and would be a panic in a plugin that assumed
    /// its sibling.
    #[test]
    fn the_notice_line_is_harmless_without_a_session() {
        let mut app = headless_ui(ConnectionState::Connecting);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        steps(&mut app, 2);

        assert_eq!(notice_line(&mut app), "");
    }

    // -------------------------------------------------------------------------
    // The frame-rate readout
    // -------------------------------------------------------------------------

    /// The readout's visibility, its node and its text, in one look.
    fn readout(app: &mut App) -> (Visibility, Node, String) {
        let world = app.world_mut();
        let mut nodes = world.query_filtered::<(&Visibility, &Node, &Text), With<ReadoutText>>();
        let found: Vec<(Visibility, Node, String)> = nodes
            .iter(world)
            .map(|(visibility, node, text)| (*visibility, node.clone(), text.0.clone()))
            .collect();
        assert_eq!(found.len(), 1, "exactly one readout node exists");
        found.into_iter().next().expect("just counted one")
    }

    #[test]
    fn the_readout_is_off_until_a_player_asks_for_it() {
        let mut app = headless_ui(ConnectionState::Connected);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        steps(&mut app, 2);
        let (visibility, _, line) = readout(&mut app);
        assert_eq!(visibility, Visibility::Hidden);
        assert_eq!(line, "", "a hidden readout still built a string");

        app.world_mut().resource_mut::<Settings>().toggle_readout();
        steps(&mut app, 1);
        let (visibility, _, line) = readout(&mut app);
        assert_eq!(visibility, Visibility::Visible);
        assert!(line.contains("fps"), "{line}");
    }

    /// A readout with no session says so rather than claiming a latency of nought.
    #[test]
    fn the_snapshot_age_is_a_dash_until_a_snapshot_has_arrived() {
        assert_eq!(describe_readout(60.0, None), "60 fps · snapshot -");
        assert_eq!(
            describe_readout(59.6, Some(Duration::from_millis(48))),
            "60 fps · snapshot 48 ms"
        );
    }

    /// It moves to the corner the setting names, and the top-left one clears the four debug
    /// lines rather than being drawn over them.
    #[test]
    fn the_readout_sits_in_the_corner_the_setting_names() {
        let mut app = headless_ui(ConnectionState::Connected);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        app.world_mut().resource_mut::<Settings>().toggle_readout();
        steps(&mut app, 1);

        let mut seen = Vec::new();
        for _ in 0..4 {
            let corner = app.world().resource::<Settings>().readout_corner();
            let (_, node, _) = readout(&mut app);
            assert_eq!(node, corner_node(corner), "{corner:?} was not applied");
            seen.push(corner);
            app.world_mut()
                .resource_mut::<Settings>()
                .cycle_readout_corner();
            steps(&mut app, 1);
        }
        seen.sort_by_key(|corner| corner.name());
        seen.dedup();
        assert_eq!(seen.len(), 4, "the four corners are not four places");

        assert_eq!(corner_node(Corner::TopLeft).top, Val::Px(READOUT_LINE));
        assert_eq!(corner_node(Corner::TopRight).top, Val::Px(MARGIN));
        const {
            assert!(
                READOUT_LINE > FOURTH_LINE,
                "the top-left readout would be drawn over the notice line"
            );
        }
    }

    /// The age is measured from the moment the tick moved, not from the frame the resource
    /// was written: `PlayerStats` is rewritten on frames when nothing in it changed, and an
    /// age read from that would be zero for ever.
    #[test]
    fn the_snapshot_age_grows_while_the_server_tick_stands_still() {
        let mut app = headless_ui(ConnectionState::Connected);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
            .insert_resource(player_stats());
        app.world_mut().resource_mut::<Settings>().toggle_readout();
        steps(&mut app, 1);
        let (_, _, first) = readout(&mut app);
        assert!(first.contains("snapshot 0 ms"), "{first}");

        // Touched but not moved: the same tick, several frames later.
        for _ in 0..3 {
            app.world_mut().resource_mut::<PlayerStats>().entities += 1;
            steps(&mut app, 1);
        }
        let (_, _, later) = readout(&mut app);
        assert!(later.contains("snapshot 300 ms"), "{later}");

        // And a new tick puts it back to nothing.
        app.world_mut().resource_mut::<PlayerStats>().server_tick = Some(1235);
        steps(&mut app, 1);
        let (_, _, fresh) = readout(&mut app);
        assert!(fresh.contains("snapshot 0 ms"), "{fresh}");
    }
}
