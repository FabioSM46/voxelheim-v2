//! The dungeon sessions window: every saved run this character is currently bound to.
//!
//! **A list, and only a list.** There is nothing to press in here — no leaving a session, no
//! resetting one, no entering from it — because every one of those is the server's answer to
//! a request made somewhere else. What this window contributes is the one thing a player
//! cannot find out any other way: which dungeons are still open to them today.
//!
//! ## The list is replaced, never merged
//!
//! [`InstanceBindings`] is complete by contract. Each one the server sends *is* the state, so
//! [`SavedRuns`] takes the newest and discards what it held — including when the newest is
//! empty, which is the case that decides the whole design. An empty list is a **statement**
//! that this character owes nothing anywhere; a window that kept its previous rows on
//! receiving one would be showing a lockout that has already reset, which is the single most
//! misleading thing this surface could do.
//!
//! That is also why a reset row leaves by *shrinking*: the server sends the whole list again
//! when a run resets, and the row is gone because it is not in the new list. Nothing here
//! compares [`SessionBinding::resets_at_unix`] against a clock and removes anything — see
//! [`reset_reading`] for what this side does with that number and what it deliberately does
//! not do with it.
//!
//! ## Neither number on a row is computed here
//!
//! `bosses_defeated` counts boss *species* the server has recorded as beaten and
//! `bosses_total` is the dungeon's boss-rank encounter count at that same granularity. The
//! client has no roster of a dungeon's bosses and never sees the corpses of a run it is not
//! in, so "1 / 2" is those two bytes laid out and nothing else.
//!
//! ## It reads the same inside an instance as outside one
//!
//! A binding names the ruin's arch in the **open world**, which is a place that exists
//! whichever world the character is standing in, and the window reads a resource rather than
//! anything about the world around it. Crossing therefore changes nothing here except by way
//! of the new list the server sends.

use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;

use super::compass::coordinates_reading;
use super::icon;
use crate::net::{InstanceBindingsInbox, MarkerKind, Session, SessionBinding};
use crate::player::{InputMode, SelfVitals};

/// The window's width in logical pixels. Wide enough for the coordinates line and the two
/// readings beside it without wrapping; narrower than the vendor's two columns, because
/// this is one column of short rows.
const WIDTH: f32 = 460.0;

const PADDING: f32 = 16.0;

/// One row's mark, in logical pixels. Square, and a little larger than the line it sits
/// beside so the arch reads at a glance down the left edge of the list.
const ROW_ICON: f32 = 28.0;

const TITLE_SIZE: f32 = 22.0;
const ROW_SIZE: f32 = 16.0;

/// Dimmer than a row, for the reason the vendor window's hint line is: the empty-state line
/// is a label about the list rather than something to read a lockout off.
const HINT: Color = Color::srgb(0.62, 0.62, 0.66);

const TITLE: &str = "Saved dungeon runs";

/// What the window says when this character owes nothing.
///
/// **An ordinary state with a plain sentence, and worded so it cannot read as a failure.**
/// It is what every character sees before their first boss kill, and a panel that looked
/// broken there would be broken for everybody on their first evening.
const NOTHING_SAVED: &str = "No saved runs. Every dungeon is open to you.";

/// Seconds in an hour and in a minute, named so [`reset_reading`]'s arithmetic reads as
/// what it is.
const HOUR: i64 = 3_600;
const MINUTE: i64 = 60;

/// The client's copy of the server's complete list, replaced wholesale on every update.
///
/// Not merged, not sorted, not filtered: the wire order is stable across two sends of
/// unchanged state, so laying the rows out in it is what makes "the same list again" look
/// the same on screen.
#[derive(Resource, Debug, Default, PartialEq, Eq)]
struct SavedRuns(Vec<SessionBinding>);

#[derive(Component)]
struct SessionsRoot;

pub(super) struct SessionsUiPlugin;

impl Plugin for SessionsUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SavedRuns>()
            .init_resource::<InputMode>()
            .init_resource::<InstanceBindingsInbox>()
            .add_systems(Startup, spawn_window)
            // `rebuild_window` spawns rows through deferred commands, so the visibility
            // system is chained behind `ApplyDeferred` for the reason the vendor window's
            // purse is: a window shown before its children exist is one empty frame.
            .add_systems(
                Update,
                (apply_bindings, rebuild_window, ApplyDeferred, show_window).chain(),
            );
    }
}

fn spawn_window(mut commands: Commands) {
    commands.spawn((
        SessionsRoot,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(WIDTH),
            max_height: Val::Percent(80.0),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
            padding: UiRect::all(Val::Px(PADDING)),
            overflow: Overflow::scroll_y(),
            ..default()
        },
        UiTransform::from_translation(Val2::percent(-50.0, -50.0)),
        BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.96)),
        // The layer the loot and vendor windows use, so this window sits with its peers
        // rather than over or under them.
        GlobalZIndex(30),
        Visibility::Hidden,
    ));
}

/// Replaces the client's copy with the newest complete list the server sent.
///
/// **The last list of the batch wins, whatever it is.** Two in one frame are two successive
/// statements of the same thing and only the newer is true; "the last non-empty one" would
/// be a merge wearing a filter, and would leave a reset lockout on the screen.
///
/// The list is also dropped when the session goes, because a binding belongs to one
/// character: the next session's own list arrives on entering the world — empty included —
/// so nothing is lost by not carrying this one across.
fn apply_bindings(
    mut inbox: ResMut<InstanceBindingsInbox>,
    session: Option<Res<Session>>,
    mut runs: ResMut<SavedRuns>,
) {
    let delivered = inbox.take();
    if session.is_none() {
        if !runs.0.is_empty() {
            runs.0.clear();
        }
        return;
    }
    let Some(newest) = delivered.into_iter().next_back() else {
        return;
    };
    if runs.0 != newest.bindings {
        runs.0 = newest.bindings;
    }
}

/// Rebuilds the whole list whenever the server's answer moves.
///
/// Rebuilt rather than reconciled, for the reason the vendor window is: the list is replaced
/// wholesale anyway, so there is no row identity to preserve and reconciling would be a
/// second opinion about which row is which.
fn rebuild_window(
    runs: Res<SavedRuns>,
    roots: Query<Entity, With<SessionsRoot>>,
    mut commands: Commands,
) {
    if !runs.is_changed() {
        return;
    }
    let now = now_unix();
    for root in &roots {
        commands.entity(root).despawn_related::<Children>();
        commands.entity(root).with_children(|root| {
            root.spawn((
                Text::new(TITLE.to_owned()),
                TextFont {
                    font_size: FontSize::Px(TITLE_SIZE),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            if runs.0.is_empty() {
                root.spawn((
                    Text::new(NOTHING_SAVED.to_owned()),
                    TextFont {
                        font_size: FontSize::Px(ROW_SIZE),
                        ..default()
                    },
                    TextColor(HINT),
                ));
                return;
            }
            for binding in &runs.0 {
                spawn_row(root, *binding, now);
            }
        });
    }
}

/// One saved run: the arch mark, then where it is and how the run stands.
///
/// **The mark is `ui/icon.rs`'s arch, not a drawing of this module's own.** That vocabulary
/// already answers "what does a way into the ground look like at twenty-odd pixels", and a
/// second answer here would be a second thing a player has to learn for the same idea. It is
/// keyed on [`MarkerKind`] because that is the key the vocabulary has; nothing about this row
/// is a map mark, and no mark is placed, read or implied by drawing one.
fn spawn_row(rows: &mut ChildSpawnerCommands<'_>, binding: SessionBinding, now: i64) {
    rows.spawn((
        Node {
            width: Val::Percent(100.0),
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(10.0),
            padding: UiRect::all(Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(super::FILLED_CELL),
    ))
    .with_children(|row| {
        row.spawn(Node {
            width: Val::Px(ROW_ICON),
            height: Val::Px(ROW_ICON),
            flex_shrink: 0.0,
            ..default()
        })
        .with_children(|mark| icon::spawn_marker(mark, MarkerKind::Cave));
        row.spawn(Node {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_width: Val::Px(0.0),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            overflow: Overflow::clip(),
            ..default()
        })
        .with_children(|lines| {
            // The compass's own line, from the compass's own function. Two copies of this
            // format would be two readings of one place that agree until one is retouched,
            // and a player reads the arch off this row to walk to it.
            lines.spawn((
                Text::new(coordinates_reading(Vec3::new(
                    binding.arch.x as f32,
                    binding.arch.y as f32,
                    binding.arch.z as f32,
                ))),
                TextFont {
                    font_size: FontSize::Px(ROW_SIZE),
                    ..default()
                },
                TextColor(Color::WHITE),
                TextLayout::no_wrap(),
            ));
            lines.spawn((
                Text::new(format!(
                    "{} | {}",
                    progress_reading(binding),
                    reset_reading(binding.resets_at_unix, now)
                )),
                TextFont {
                    font_size: FontSize::Px(ROW_SIZE),
                    ..default()
                },
                TextColor(HINT),
                TextLayout::no_wrap(),
            ));
        });
    });
}

/// How the two boss counts read on a row.
///
/// The server's two bytes, laid out. No total is computed, supplemented or clamped here —
/// `schemas/player.fbs` says these count boss *species* and names the server as the one
/// place that must learn the difference when a dungeon holds two bosses of one kind.
fn progress_reading(binding: SessionBinding) -> String {
    format!(
        "{} / {} bosses",
        binding.bosses_defeated, binding.bosses_total
    )
}

/// How a reset reads, given the wall clock this machine believes.
///
/// **A reading, never a decision.** The contract is explicit that a client must not expire a
/// binding itself: the server sends the whole list again when a run resets, and a row leaves
/// because it is absent from that list. So a reset the local clock has already passed says
/// `resets shortly` and the row stays — the alternative is a window that hides a lockout the
/// server still holds because this machine's clock is fast.
///
/// Whole hours and minutes, because a binding lasts until the server's next midnight and a
/// count of seconds is not what anybody asks. Under a minute rounds down to `resets shortly`
/// for the same reason.
fn reset_reading(resets_at_unix: i64, now: i64) -> String {
    let remaining = resets_at_unix.saturating_sub(now);
    if remaining < MINUTE {
        return "resets shortly".to_owned();
    }
    let hours = remaining / HOUR;
    let minutes = (remaining % HOUR) / MINUTE;
    if hours == 0 {
        return format!("resets in {minutes}m");
    }
    format!("resets in {hours}h {minutes}m")
}

/// This machine's Unix second, or zero when the clock is before the epoch.
///
/// Zero is a safe answer for the one thing it is used for: it makes every reset read as
/// further away than it is, which leaves a row on the screen rather than taking one off.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Shows the window exactly while [`InputMode::Sessions`] owns the pointer.
///
/// The vendor window's rule with one clause fewer: there is no open stall to be holding, so
/// a live session and a living character are the whole of it. Death closing it is
/// presentation and not a decision — a corpse owes exactly what it owed standing up — and it
/// is here because a list on top of a death overlay is a screen nobody can read.
fn show_window(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    vitals: Option<Res<SelfVitals>>,
    mut roots: Query<&mut Visibility, With<SessionsRoot>>,
) {
    let dead = vitals.is_some_and(|vitals| vitals.dead());
    let shown = session.is_some() && !dead && *mode == InputMode::Sessions;
    for mut visibility in &mut roots {
        let next = if shown {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != next {
            *visibility = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, BlockCoord, InstanceBindings, SessionParams};

    /// Midnight UTC on 1 January 2030, which is a legal reset and far enough from any
    /// clock this test could run under that a countdown reading is never ambiguous.
    const RESET: i64 = 1_893_456_000;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.5, 64.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    fn binding(x: i32, z: i32, defeated: u8, total: u8) -> SessionBinding {
        SessionBinding {
            arch: BlockCoord { x, y: 61, z },
            bosses_defeated: defeated,
            bosses_total: total,
            resets_at_unix: RESET,
        }
    }

    /// An app with the window built, a live session, and the window open.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(InputMode::Sessions)
            .add_plugins(SessionsUiPlugin);
        app.update();
        app
    }

    /// Delivers one complete list the way the net boundary does, and runs a frame.
    fn deliver(app: &mut App, bindings: Vec<SessionBinding>) {
        app.world_mut()
            .resource_mut::<InstanceBindingsInbox>()
            .push(InstanceBindings { bindings });
        app.update();
    }

    fn lines(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        let mut texts = world.query::<&Text>();
        texts.iter(world).map(|text| text.0.clone()).collect()
    }

    fn visible(app: &mut App) -> bool {
        let world = app.world_mut();
        let mut roots = world.query_filtered::<&Visibility, With<SessionsRoot>>();
        *roots.single(world).unwrap() == Visibility::Visible
    }

    /// The list is replaced and never merged — including when it shrinks, and including
    /// when it shrinks to nothing.
    ///
    /// **The shrink is the case worth the test.** A window that merged would keep the
    /// dungeon that reset, and the player would plan an evening around a lockout the
    /// server had already released. The empty answer at the end is the same defect one
    /// step further: it is a statement that this character owes nothing anywhere, and a
    /// consumer that read it as "no news" would leave every row standing.
    #[test]
    fn a_new_list_replaces_the_old_one_whole_and_a_reset_run_leaves_the_window() {
        let mut app = app();

        deliver(
            &mut app,
            vec![binding(-4096, 8192, 1, 2), binding(512, -64, 0, 3)],
        );
        let drawn = lines(&mut app);
        assert!(
            drawn.contains(&"X -4096 | Z 8192 | alt 61".to_owned()),
            "the arch a player walks to is on the row: {drawn:?}"
        );
        assert!(
            drawn.iter().any(|line| line.starts_with("1 / 2 bosses | ")),
            "the server's two counts are laid out and neither is recomputed: {drawn:?}"
        );
        assert!(
            drawn.iter().any(|line| line.starts_with("0 / 3 bosses | ")),
            "{drawn:?}"
        );

        // One of the two runs reset. The server sends the whole list again, shorter.
        deliver(&mut app, vec![binding(512, -64, 0, 3)]);
        let drawn = lines(&mut app);
        assert!(
            !drawn.contains(&"X -4096 | Z 8192 | alt 61".to_owned()),
            "a run absent from the newest list is not owed and must leave: {drawn:?}"
        );
        assert!(
            drawn.contains(&"X 512 | Z -64 | alt 61".to_owned()),
            "{drawn:?}"
        );

        // And the last one reset too. An empty list is an answer, not a silence.
        deliver(&mut app, Vec::new());
        let drawn = lines(&mut app);
        assert!(
            !drawn.iter().any(|line| line.contains(" bosses ")),
            "an empty list clears every row: {drawn:?}"
        );
        assert!(drawn.contains(&NOTHING_SAVED.to_owned()), "{drawn:?}");
    }

    /// The empty state is what every character sees before their first boss kill, and it
    /// is an ordinary panel with a plain sentence rather than a blank or a warning.
    #[test]
    fn a_character_who_owes_nothing_gets_a_plain_sentence_and_a_drawn_panel() {
        let mut app = app();

        let drawn = lines(&mut app);
        assert!(drawn.contains(&TITLE.to_owned()), "{drawn:?}");
        assert!(drawn.contains(&NOTHING_SAVED.to_owned()), "{drawn:?}");
        assert!(visible(&mut app), "the empty window is still a window");

        // An empty list arriving over an empty window changes nothing and is not an error.
        deliver(&mut app, Vec::new());
        assert!(lines(&mut app).contains(&NOTHING_SAVED.to_owned()));
    }

    /// Two lists in one frame are two successive statements, and only the newer is true.
    ///
    /// The newer one here is empty, which is the ordering a "keep the last non-empty"
    /// reading would get exactly backwards.
    #[test]
    fn two_lists_in_one_frame_leave_only_the_newer_on_the_screen() {
        let mut app = app();
        let inbox = &mut app.world_mut().resource_mut::<InstanceBindingsInbox>();
        inbox.push(InstanceBindings {
            bindings: vec![binding(-4096, 8192, 1, 2)],
        });
        inbox.push(InstanceBindings::default());
        app.update();

        let drawn = lines(&mut app);
        assert!(
            drawn.contains(&NOTHING_SAVED.to_owned()),
            "the newer, empty list is the one that stands: {drawn:?}"
        );
    }

    /// The window is drawn exactly while its mode owns the pointer, and the session's end
    /// takes the list with it.
    ///
    /// A binding belongs to one character, so carrying a list across a session boundary
    /// would be this client showing one character's lockouts to another. The next
    /// session's own list arrives on entering the world — empty included — so nothing is
    /// lost by dropping this one.
    #[test]
    fn the_window_follows_its_mode_and_the_list_does_not_outlive_the_session() {
        let mut app = app();
        deliver(&mut app, vec![binding(-4096, 8192, 1, 2)]);
        assert!(visible(&mut app));

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert!(!visible(&mut app), "the window closes with its mode");

        app.world_mut().remove_resource::<Session>();
        app.update();
        assert_eq!(
            app.world().resource::<SavedRuns>().0,
            Vec::new(),
            "a lockout does not survive the character that owed it"
        );
        assert!(lines(&mut app).contains(&NOTHING_SAVED.to_owned()));
    }

    /// A reset is read, and a reset the local clock has passed is still read rather than
    /// acted on.
    ///
    /// **The last row is the one that matters.** `schemas/player.fbs` is explicit that a
    /// client must not expire a binding itself: a binding lasts until the server's next
    /// midnight, the server sends the whole list again when it goes, and a window that
    /// dropped a row on its own clock would hide a lockout the server still holds.
    #[test]
    fn a_reset_is_a_reading_and_never_an_expiry() {
        for (name, resets_at, now, want) in [
            (
                "a whole day out",
                RESET,
                RESET - 24 * HOUR,
                "resets in 24h 0m",
            ),
            (
                "hours and minutes",
                RESET,
                RESET - 7 * HOUR - 12 * MINUTE,
                "resets in 7h 12m",
            ),
            ("under an hour", RESET, RESET - 45 * MINUTE, "resets in 45m"),
            ("under a minute", RESET, RESET - 30, "resets shortly"),
            ("exactly now", RESET, RESET, "resets shortly"),
            (
                "already past on this clock",
                RESET,
                RESET + 9 * HOUR,
                "resets shortly",
            ),
        ] {
            assert_eq!(reset_reading(resets_at, now), want, "{name}");
        }
    }
}
