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
    ActionRefused, ConnectionState, Identity, MAX_MARKERS, RefusalInbox, RefusalReason,
    RefusedAction, Reject, ServerAddress, Session,
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

/// How often the measured values are published to the visible text.
///
/// Four readings per second keep the label recent without presenting every rounded frame-rate
/// change and every millisecond of snapshot age as a new string. Sampling remains per-frame, so
/// the smoothing still sees every measurement; this interval bounds presentation only.
const READOUT_PUBLICATION_INTERVAL: Duration = Duration::from_millis(250);

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
            .init_resource::<ReadoutMeasurements>()
            .init_resource::<Settings>()
            .add_systems(Startup, spawn_status_text)
            .add_systems(
                Update,
                (
                    refresh_status_text,
                    refresh_world_text,
                    refresh_player_text,
                    refresh_notice_text,
                    (sample_readout, refresh_readout).chain(),
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

/// The newest values the readout may publish.
///
/// Measurement stays separate from presentation so every frame contributes to smoothing while
/// the text itself changes only on the bounded cadence below.
#[derive(Resource, Debug, Default)]
struct ReadoutMeasurements {
    smoothed_frame_rate: f32,
    newest_snapshot: Option<(u32, Duration)>,
}

/// Presentation-only cadence and the edge that makes enabling immediate.
struct ReadoutPublication {
    cadence: Timer,
    was_visible: bool,
}

impl Default for ReadoutPublication {
    fn default() -> Self {
        Self {
            cadence: Timer::new(READOUT_PUBLICATION_INTERVAL, TimerMode::Repeating),
            was_visible: false,
        }
    }
}

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
    pub(super) fn line(&self) -> &str {
        if self.until.is_some() { &self.line } else { "" }
    }

    /// Shows `line` for [`NOTICE_LIFETIME`] from `now`, replacing whatever was there.
    ///
    /// Replaces rather than queues, for the reason the inbox keeps the newest: two
    /// refusals are two different answers, and the older one is about a press the player
    /// has already stopped thinking about.
    pub(super) fn show(&mut self, line: String, now: Duration) {
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
    format!("{frame_rate:.0} fps | snapshot {age}")
}

/// Samples the two values the readout presents on every frame.
///
/// Both counters are measured here rather than read from a resource somebody else keeps: the
/// frame rate is this schedule's own `Time`, and the snapshot age anchor is the moment
/// `PlayerStats.server_tick` last moved. Neither is a decision and neither is on the wire.
fn sample_readout(
    time: Option<Res<Time>>,
    stats: Option<Res<PlayerStats>>,
    mut measurements: ResMut<ReadoutMeasurements>,
) {
    let Some(time) = time else {
        return;
    };
    let now = time.elapsed();

    let delta = time.delta_secs();
    if delta > 0.0 {
        let instant = 1.0 / delta;
        measurements.smoothed_frame_rate = if measurements.smoothed_frame_rate > 0.0 {
            measurements.smoothed_frame_rate
                + (instant - measurements.smoothed_frame_rate) * FRAME_RATE_SMOOTHING
        } else {
            instant
        };
    }

    // The tick, not the resource's change flag: `PlayerStats` is rewritten on frames when
    // nothing about it moved, and an age measured from that would read zero for ever.
    if let Some(tick) = stats.as_ref().and_then(|stats| stats.server_tick)
        && measurements.newest_snapshot.map(|(held, _)| held) != Some(tick)
    {
        measurements.newest_snapshot = Some((tick, now));
    }
}

/// Keeps the readout current, and out of the way when it is switched off.
///
/// Visibility and placement follow their settings immediately. The string is published only
/// once per [`READOUT_PUBLICATION_INTERVAL`], except that switching the readout on publishes the
/// first readable value in that same frame.
fn refresh_readout(
    time: Option<Res<Time>>,
    settings: Res<Settings>,
    measurements: Res<ReadoutMeasurements>,
    mut nodes: Query<(&mut Text, &mut Node, &mut Visibility), With<ReadoutText>>,
    mut publication: Local<ReadoutPublication>,
) {
    let Some(time) = time else {
        return;
    };

    let shown = if settings.readout_shown() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let placement = corner_node(settings.readout_corner());
    let visible = shown == Visibility::Visible;
    let became_visible = visible && !publication.was_visible;
    let publish = if !visible {
        publication.cadence.reset();
        false
    } else if became_visible {
        publication.cadence.reset();
        true
    } else {
        publication.cadence.tick(time.delta()).just_finished()
    };
    publication.was_visible = visible;

    let line = publish.then(|| {
        let age = measurements
            .newest_snapshot
            .map(|(_, at)| time.elapsed().saturating_sub(at));
        describe_readout(measurements.smoothed_frame_rate, age)
    });

    for (mut text, mut node, mut visibility) in &mut nodes {
        if *visibility != shown {
            *visibility = shown;
        }
        if *node != placement {
            *node = placement.clone();
        }
        if let Some(line) = &line
            && text.0 != *line
        {
            text.0.clone_from(line);
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
            // A silence this build chose: the reason is named and decoded, and the
            // surface that will answer it does not exist yet. Logging it would put a line
            // per refusal in front of an operator for a decision already recorded in
            // `has_no_sentence_yet`.
            None if has_no_sentence_yet(refused.reason) => {}
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
/// Three kinds of `None`, and all three are silence on purpose:
///
///   - a reason that says the *request* was wrong. A correct client never produces one,
///     so it is a defect in this build; the player did nothing and can do nothing.
///   - a reason or an action this build cannot name, which is a server one contract
///     ahead. There is no sentence to write that would not be a guess.
///   - a reason this build *can* name and has deliberately not written a sentence for,
///     because the surface that would answer it does not exist yet — see
///     [`has_no_sentence_yet`]. The third kind was added by V25; before it, the two above
///     were stated as exhaustive and a reason like `NotAVendor` fell through the second,
///     which reported a contract this build had read as one it had not.
///
/// ASCII only, for the reason [`describe`] is: the embedded fallback font is the whole
/// font stack, and a glyph it lacks renders as nothing.
/// Whether this build can name the reason and has deliberately written no sentence for it.
///
/// **Not a defect, and not an unreadable code** — the two silences that existed before
/// V25. These are reasons the contract names, this build decodes, and no surface answers
/// yet. `TileMisaligned` is the only one left: a misaligned tile request is this build
/// asking wrongly, but it is not one of the four `Malformed*` codes
/// [`RefusalReason::is_client_defect`] names, so nothing classified it at all.
///
/// It exists because the sweep needs a third category to assert against, and because the
/// caller needs one to keep from logging "no sentence for" at a reason whose silence is a
/// decision. **A reason leaves this list when something answers it**, which is what #458
/// did for `NotAVendor` — F addresses a resident, so the answer has somewhere to be read —
/// and what #459 has now done for `NotEnoughSilver` and `VendorDoesNotWant`: there is a
/// stall on screen, and a refused trade is answered beside it.
///
/// One name is left, so [`the_deliberate_silences_do_not_overlap`]'s non-empty assertion
/// still holds. It will not survive the issue that gives `TileMisaligned` a surface, and
/// that test says so rather than the category quietly becoming decorative.
fn has_no_sentence_yet(reason: RefusalReason) -> bool {
    matches!(reason, RefusalReason::TileMisaligned)
}

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
        // V26's one, and the only new reason that *is* about a placement. It names no
        // owner because the contract does not carry one: a line saying whose ward it was
        // would be this build inventing the half the server deliberately withheld.
        RefusalReason::Warded => Some("this ground is warded"),
        RefusalReason::TooFast
        | RefusalReason::PartyFull
        | RefusalReason::NoSuchPlayer
        | RefusalReason::AlreadyInParty
        | RefusalReason::NoInvite
        | RefusalReason::NotLeader
        | RefusalReason::CorpseUnavailable
        | RefusalReason::LootNotOwned
        | RefusalReason::StaleRevision
        | RefusalReason::InventoryFull
        | RefusalReason::NoAmmunition
        // V24's four map refusals. None of them is about a placement: the three that are
        // about a mark are answered by `marker_reason` below, and `TileMisaligned` still has
        // no sentence anywhere -- a misaligned tile request is a defect in this build, and
        // the player did not ask for it.
        | RefusalReason::TileMisaligned
        | RefusalReason::TooManyMarkers
        | RefusalReason::NoteTooLong
        | RefusalReason::MarkerUnknown
        // V25's three settlement refusals. None of them is about a placement either:
        // `NotAVendor` and the two trade reasons are all answered below, against the
        // actions that produce them.
        | RefusalReason::NotAVendor
        | RefusalReason::NotEnoughSilver
        | RefusalReason::VendorDoesNotWant
        | RefusalReason::Unknown
        | RefusalReason::MalformedNoAnchor
        | RefusalReason::MalformedFacing
        | RefusalReason::MalformedSlot
        | RefusalReason::MalformedKind => None,
    };
    let loot_reason = match refused.reason {
        RefusalReason::CorpseUnavailable => Some("that corpse is no longer available"),
        RefusalReason::LootNotOwned => Some("those spoils are not yours"),
        RefusalReason::OutOfReach => Some("that corpse is too far away"),
        RefusalReason::StaleRevision => Some("the contents changed; review them and try again"),
        RefusalReason::InventoryBusy => Some("your pack was busy; try again"),
        RefusalReason::InventoryFull => Some("your pack is full"),
        RefusalReason::PlayerIsDead => Some("you cannot loot while dead"),
        _ => None,
    };

    // The three a mark can be refused for. Whole sentences rather than the "Cannot X: y"
    // shape the placement and party lines use, because the map is not somewhere a player is
    // trying to do a thing to the world -- it is their own notebook, and a full one has a
    // number worth reading.
    //
    // `NoteTooLong` is here and cannot arrive: the decoder refuses an over-long note by
    // closing the session, so the server's own bound is unreachable over the wire. It is
    // written anyway for the reason `session/markers.go` checks it anyway -- this is the
    // sentence a client would be told if that ever stopped being true, and a reason with no
    // line is a refusal that reaches nobody.
    let marker_reason = match refused.reason {
        RefusalReason::TooManyMarkers => {
            Some(format!("The map holds no more marks ({MAX_MARKERS})"))
        }
        RefusalReason::NoteTooLong => Some("That note is too long".to_owned()),
        RefusalReason::MarkerUnknown => Some("That mark is already gone".to_owned()),
        _ => None,
    };

    // Every reason `Player.Trade` can answer with, in the whole-sentence shape the map's
    // and the interact line's use: nothing was refused that the player meant to do *to the
    // world*, so "Cannot build here: x" would be the wrong frame for it.
    //
    // **Seven, not the three the issue names.** The three are the ones a player meets by
    // playing badly; the other four are how a stall ends underneath them — walking out of
    // reach, dying, clicking a list the server has already replaced, and a pack another
    // request holds the lock on. A reason that reached this arm with no line would be
    // logged as "this build has no sentence for", which is the log entry reserved for a
    // *defect*, and none of the four is one.
    let trade_reason = match refused.reason {
        RefusalReason::NotEnoughSilver => Some("Not enough silver"),
        RefusalReason::VendorDoesNotWant => Some("They do not want that"),
        RefusalReason::InventoryFull => Some("Your pack is full"),
        RefusalReason::NotAVendor => Some("That stall is closed"),
        RefusalReason::StaleRevision => Some("The prices changed; look again"),
        RefusalReason::InventoryBusy => Some("Your pack was busy; try again"),
        RefusalReason::PlayerIsDead => Some("You cannot trade while dead"),
        _ => None,
    };

    match (refused.action, refused.reason) {
        (RefusedAction::PlaceMarker | RefusedAction::RemoveMarker, _) => marker_reason,
        (RefusedAction::Trade, _) => trade_reason.map(str::to_owned),
        (RefusedAction::Attack, RefusalReason::NoAmmunition) => Some("No arrows".to_owned()),
        // A whole sentence about the person rather than the "Cannot X: y" shape, for the
        // reason the map's lines are whole sentences: nothing was refused that the player
        // meant to do to the world. They addressed somebody, and the answer is about that
        // somebody.
        //
        // **It says what is true today and no more.** The server answers `NotAVendor` on
        // every path — an unknown entity, one out of its own reach, a guard, and a smith
        // whose price list #459 has not written yet — so a line naming any of those four
        // would be this build inventing which one it was. "They have nothing to trade" is
        // the one sentence all four support.
        (RefusedAction::Interact, RefusalReason::NotAVendor) => {
            Some("They have nothing to trade".to_owned())
        }
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
        (RefusedAction::OpenLoot | RefusedAction::TakeLoot, _) => {
            loot_reason.map(|reason| format!("Cannot loot: {reason}"))
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
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
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
    /// that `match`.
    ///
    /// **What keeps it complete is [`every_reason_is_in_the_sweep`], not the compiler.**
    /// This comment used to claim a reason appended later "has to be extended by hand to
    /// compile past it", and that is simply false: a fixed-size array of enum values does
    /// not stop compiling when the enum grows. It cost two batches. V24's four map reasons
    /// and V25's three settlement ones were both appended without being added here, so
    /// every sweep below ran over 27 of 34 members while reading as though it swept them
    /// all — and a wrong sentence for any of the seven was green. The length assert is
    /// what the old comment only promised.
    const EVERY_REASON: [RefusalReason; 35] = [
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
        RefusalReason::CorpseUnavailable,
        RefusalReason::LootNotOwned,
        RefusalReason::StaleRevision,
        RefusalReason::InventoryFull,
        RefusalReason::NoAmmunition,
        // V24's four map reasons.
        RefusalReason::TileMisaligned,
        RefusalReason::TooManyMarkers,
        RefusalReason::NoteTooLong,
        RefusalReason::MarkerUnknown,
        // V25's three settlement reasons.
        RefusalReason::NotAVendor,
        RefusalReason::NotEnoughSilver,
        RefusalReason::VendorDoesNotWant,
        // V26's one warded-ground reason.
        RefusalReason::Warded,
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
            RefusalReason::NoAmmunition => RefusedAction::Attack,
            // Their sentences live behind a marker action, so a placement action here
            // would sweep them as silent and pin the opposite of what is true.
            RefusalReason::TooManyMarkers
            | RefusalReason::NoteTooLong
            | RefusalReason::MarkerUnknown => RefusedAction::PlaceMarker,
            RefusalReason::CorpseUnavailable
            | RefusalReason::LootNotOwned
            | RefusalReason::StaleRevision
            | RefusalReason::InventoryFull => RefusedAction::OpenLoot,
            // Its sentence lives behind the interact action, so a placement action here
            // would sweep it as silent and pin the opposite of what is true — exactly the
            // trap the marker three are lifted out of above.
            RefusalReason::NotAVendor => RefusedAction::Interact,
            // And the same trap for the two #459 lifted out of `has_no_sentence_yet`: the
            // `_` arm below sent both in as `PlaceStructure`, which has no sentence for
            // either, so the sweep would have kept passing over the pair while reading as
            // though it had checked them.
            RefusalReason::NotEnoughSilver | RefusalReason::VendorDoesNotWant => {
                RefusedAction::Trade
            }
            _ => RefusedAction::PlaceStructure,
        };
        ActionRefused {
            action,
            reason,
            anchor: Some(crate::net::BlockCoord { x: 0, y: 63, z: 0 }),
        }
    }

    /// [`EVERY_REASON`] is every reason, and a length is what says so.
    ///
    /// The sweeps in this module are only as good as the list they run over, and nothing
    /// in the language makes that list complete: a `[RefusalReason; N]` compiles perfectly
    /// while the enum grows past `N`. That is not hypothetical — it happened twice, in
    /// V24 and again in V25, and both times every sweep below went on passing over a
    /// subset. `fb::RefusalReason::ENUM_VALUES` is the contract's own count, which is the
    /// same pin the codec puts on `CLASSIFICATION`.
    #[test]
    fn every_reason_is_in_the_sweep() {
        assert_eq!(
            EVERY_REASON.len(),
            crate::wire::voxelheim::net::RefusalReason::ENUM_VALUES.len(),
            "a reason the contract names is missing from EVERY_REASON, so every sweep in \
             this module is reporting on a subset while reading as if it swept them all"
        );

        // A length on its own would be satisfied by naming one member twice while another
        // stayed missing, which is the same hole one step in.
        for (seen, reason) in EVERY_REASON.iter().enumerate() {
            assert!(
                !EVERY_REASON[..seen].contains(reason),
                "{reason:?} appears twice in EVERY_REASON, so some other reason is absent"
            );
        }
    }

    /// Every reason either becomes a sentence or is deliberately shown to nobody.
    ///
    /// The three silences are not the same thing and all of them are on purpose:
    ///
    ///   - a reason that says the *request* was wrong. A correct client never produces one,
    ///     so it is this build's own defect; the player did nothing and can do nothing.
    ///   - `Unknown`, which is a server one contract ahead. Writing a sentence for a code
    ///     this build cannot read would present a guess as the server's answer.
    ///   - a reason this build can name whose answering surface is not written yet, listed
    ///     in [`has_no_sentence_yet`]. **The third category is not a widening to make this
    ///     test pass** — it is the category V24 and V25 both landed in while
    ///     [`EVERY_REASON`] was too short to notice. With the seven restored, this test
    ///     fails on `TileMisaligned` before it ever reaches V25's three: the invariant had
    ///     been outgrown one contract earlier and nothing said so.
    ///
    /// A reason appended later lands in none of the three, so it has to be given a
    /// sentence or a place in one of the two lists — which is the whole point of sweeping
    /// the list rather than spot-checking it.
    #[test]
    fn every_reason_is_either_a_sentence_or_a_deliberate_silence() {
        for reason in EVERY_REASON {
            let shown = describe_refusal(&refusal(reason));
            let silent = reason == RefusalReason::Unknown
                || reason.is_client_defect()
                || has_no_sentence_yet(reason);
            assert_eq!(
                shown.is_none(),
                silent,
                "{reason:?} -> {shown:?}; silence is for this build's own defects, for \
                 codes it cannot read, and for reasons whose surface is not written yet, \
                 and for nothing else"
            );
            if let Some(line) = shown {
                // The map's three are whole sentences rather than the "Cannot X: y" shape,
                // deliberately: a notebook that is full has a number worth reading.
                assert!(
                    line.starts_with("Cannot ")
                        || line == "No arrows"
                        || line.starts_with("The map holds no more marks")
                        || line == "That note is too long"
                        || line == "That mark is already gone"
                        || line == "They have nothing to trade"
                        || line == "Not enough silver"
                        || line == "They do not want that",
                    "{reason:?} -> {line}"
                );
            }
        }
    }

    /// The three silences are three, and no reason is in two of them at once.
    ///
    /// [`has_no_sentence_yet`] is the category the test above checks against, so a member
    /// slipped into it is a member excused from ever getting a sentence. It must not
    /// overlap the defects — those are silent for a different reason and stay silent
    /// forever, while every name here is one #458 or #459 removes.
    #[test]
    fn the_deliberate_silences_do_not_overlap() {
        for reason in EVERY_REASON {
            assert!(
                !(has_no_sentence_yet(reason) && reason.is_client_defect()),
                "{reason:?} is both a client defect and a reason awaiting a surface"
            );
            assert!(
                !(has_no_sentence_yet(reason) && reason == RefusalReason::Unknown),
                "Unknown is silent because it cannot be read, not because nobody wrote it"
            );
        }
        // One name is left after #458 and #459 took theirs out — `TileMisaligned` — and
        // when the issue that gives it a surface empties the list this test says so rather
        // than the category quietly becoming decorative.
        assert!(
            EVERY_REASON.iter().copied().any(has_no_sentence_yet),
            "no reason awaits a surface; delete `has_no_sentence_yet` and its category"
        );
    }

    /// **Every reason one refused trade can come back as, pinned to its exact sentence.**
    ///
    /// The sweep above sees only one action per reason, so it checks two of these seven
    /// and cannot see the other five at all: `InventoryFull` is swept as a loot refusal,
    /// `NotAVendor` as an interact one, and `InventoryBusy` and `PlayerIsDead` as
    /// placements. This is where the `RefusedAction::Trade` arm is actually read, and the
    /// three the issue names are spelled out rather than matched loosely — they are the
    /// whole of what a player is told about a trade that did not happen.
    #[test]
    fn every_trade_refusal_has_its_own_sentence() {
        for (reason, want) in [
            (RefusalReason::NotEnoughSilver, "Not enough silver"),
            (RefusalReason::VendorDoesNotWant, "They do not want that"),
            (RefusalReason::InventoryFull, "Your pack is full"),
            (RefusalReason::NotAVendor, "That stall is closed"),
            (
                RefusalReason::StaleRevision,
                "The prices changed; look again",
            ),
            (
                RefusalReason::InventoryBusy,
                "Your pack was busy; try again",
            ),
            (RefusalReason::PlayerIsDead, "You cannot trade while dead"),
        ] {
            let refused = ActionRefused {
                action: RefusedAction::Trade,
                reason,
                anchor: None,
            };
            assert_eq!(describe_refusal(&refused).as_deref(), Some(want));
        }
    }

    /// Addressing somebody who keeps no stall says so, in those words.
    ///
    /// Pinned to the exact sentence rather than to "something non-empty", because this is
    /// the whole of what a player is told when F does nothing: the request left, the server
    /// answered, and this line is the only evidence of either.
    ///
    /// The pair matters as much as the reason. `NotAVendor` against a *placement* is still
    /// silent — it cannot arrive that way, and inventing a line for a combination no server
    /// produces is how a status line starts describing a world that is not there.
    #[test]
    fn addressing_somebody_with_no_stall_says_they_have_nothing_to_trade() {
        assert_eq!(
            describe_refusal(&ActionRefused {
                action: RefusedAction::Interact,
                reason: RefusalReason::NotAVendor,
                anchor: None,
            })
            .as_deref(),
            Some("They have nothing to trade")
        );
        assert_eq!(
            describe_refusal(&ActionRefused {
                action: RefusedAction::PlaceStructure,
                reason: RefusalReason::NotAVendor,
                anchor: None,
            }),
            None
        );
    }

    #[test]
    fn a_bow_refused_for_ammunition_says_no_arrows_exactly() {
        assert_eq!(
            describe_refusal(&ActionRefused {
                action: RefusedAction::Attack,
                reason: RefusalReason::NoAmmunition,
                anchor: None,
            }),
            Some("No arrows".to_owned())
        );
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

    #[test]
    fn loot_refusals_tell_the_player_what_to_do_or_why_the_action_stopped() {
        for (reason, fragment) in [
            (RefusalReason::CorpseUnavailable, "no longer available"),
            (RefusalReason::LootNotOwned, "not yours"),
            (RefusalReason::OutOfReach, "too far away"),
            (RefusalReason::StaleRevision, "review them and try again"),
            (RefusalReason::InventoryBusy, "try again"),
            (RefusalReason::InventoryFull, "pack is full"),
            (RefusalReason::PlayerIsDead, "while dead"),
        ] {
            for action in [RefusedAction::OpenLoot, RefusedAction::TakeLoot] {
                let line = describe_refusal(&ActionRefused {
                    action,
                    reason,
                    anchor: None,
                })
                .expect("a loot refusal has a sentence");
                assert!(line.contains(fragment), "{action:?}/{reason:?}: {line}");
            }
        }
    }

    /// The three refusals a mark can draw, and the one it cannot.
    ///
    /// `NoteTooLong` is in the list and cannot arrive over the wire -- the decoder closes the
    /// session over an over-long note before the store ever answers -- so what is pinned here
    /// is that the sentence exists rather than that a player will read it. A reason the server
    /// names and this client has no line for is a refusal that reaches nobody.
    #[test]
    fn a_refused_mark_says_which_of_the_three_things_went_wrong() {
        for (reason, want) in [
            (
                RefusalReason::TooManyMarkers,
                "The map holds no more marks (64)",
            ),
            (RefusalReason::NoteTooLong, "That note is too long"),
            (RefusalReason::MarkerUnknown, "That mark is already gone"),
        ] {
            for action in [RefusedAction::PlaceMarker, RefusedAction::RemoveMarker] {
                let line = describe_refusal(&ActionRefused {
                    action,
                    reason,
                    anchor: None,
                });
                assert_eq!(line.as_deref(), Some(want), "{action:?}/{reason:?}");
            }
        }

        // A reason that is about a tile rather than a mark still says nothing: a misaligned
        // request is a defect in this build and the player did not make it.
        assert_eq!(
            describe_refusal(&ActionRefused {
                action: RefusedAction::PlaceMarker,
                reason: RefusalReason::TileMisaligned,
                anchor: None,
            }),
            None
        );
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
        assert_eq!(describe_readout(60.0, None), "60 fps | snapshot -");
        assert_eq!(
            describe_readout(59.6, Some(Duration::from_millis(48))),
            "60 fps | snapshot 48 ms"
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

    /// Sub-interval frames keep the string byte-for-byte stable, then one publication tick
    /// refreshes both measurements together.
    #[test]
    fn the_readout_publishes_once_per_bounded_interval() {
        assert_eq!(READOUT_PUBLICATION_INTERVAL, Duration::from_millis(250));

        let mut app = headless_ui(ConnectionState::Connected);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
            .insert_resource(player_stats());
        app.world_mut().resource_mut::<Settings>().toggle_readout();
        steps(&mut app, 1);
        let (_, _, first) = readout(&mut app);
        assert!(first.contains("snapshot 0 ms"), "{first}");

        // Touched but not moved: the same tick and two advancing render frames do not
        // republish either counter before the presentation interval.
        for _ in 0..2 {
            app.world_mut().resource_mut::<PlayerStats>().entities += 1;
            steps(&mut app, 1);
            assert_eq!(readout(&mut app).2, first);
        }

        // The third 100 ms frame crosses the 250 ms interval. Repeating cadence keeps the
        // remainder, but one frame still produces at most one current publication.
        app.world_mut().resource_mut::<PlayerStats>().entities += 1;
        steps(&mut app, 1);
        let (_, _, later) = readout(&mut app);
        assert!(later.contains("snapshot 300 ms"), "{later}");
        assert_ne!(later, first);

        // The age anchor is still the moment the server tick moved, not the frame
        // `PlayerStats` was otherwise rewritten. A fresh tick waits for the same cadence
        // and honestly includes the time spent waiting to publish.
        app.world_mut().resource_mut::<PlayerStats>().server_tick = Some(1235);
        steps(&mut app, 1);
        assert_eq!(
            readout(&mut app).2,
            later,
            "published between cadence ticks"
        );
        steps(&mut app, 1);
        let (_, _, fresh) = readout(&mut app);
        assert!(fresh.contains("snapshot 100 ms"), "{fresh}");
    }

    /// Sampling continues on every render frame even though presentation does not. A sustained
    /// change therefore reaches the smoothed value well inside one second rather than waiting on
    /// a second measurement cadence.
    #[test]
    fn smoothed_fps_responds_to_a_sustained_change_within_one_second() {
        let mut app = headless_ui(ConnectionState::Connected);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));
        steps(&mut app, 1);
        app.world_mut().resource_mut::<Settings>().toggle_readout();
        steps(&mut app, 1);
        assert!(readout(&mut app).2.starts_with("10 fps"));

        *app.world_mut().resource_mut::<TimeUpdateStrategy>() =
            TimeUpdateStrategy::ManualDuration(Duration::from_millis(16));
        steps(&mut app, 62); // 992 ms of sustained ~62 fps frames.

        let (_, _, line) = readout(&mut app);
        assert!(line.starts_with("62 fps"), "{line}");
    }
}
