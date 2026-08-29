//! The countdown that runs while a leave is being honoured and the character is still
//! standing in the world.
//!
//! **Nothing here decides anything, and one line of `schemas/player.fbs` is why.** The
//! server owns the linger: `LeaveRequest` states that the client "cannot name a duration,
//! an end time, or a cancellation", and `LeaveStarted.remaining_ms` is the only place a
//! duration comes from. So this module renders [`ConnectionState::Leaving`] and does
//! nothing else with it:
//!
//! - The number on screen is `seconds_remaining` exactly as `net/` published it. No
//!   [`Timer`], no [`Time`], no interpolation between frames -- and the absence is
//!   structural rather than promised, because neither appears in this module's system
//!   parameters at all. A frame in which nothing arrives holds the last authoritative
//!   number rather than running it down.
//! - `seconds_remaining: None` is a real state with its own wording, not a zero and not a
//!   guess. It is the interval between asking to leave and the server acknowledging, and
//!   the overlay says so instead of inventing a first number.
//! - Reaching zero completes nothing. The session ends when the socket closes, which
//!   arrives as the net thread's `Ended` event and takes this state away with it; the
//!   overlay disappears on *any* exit from `Leaving`, including one that arrives while the
//!   count is still high.
//!
//! **The status line keeps its own sentence** (`ui/status.rs`). This is a second, larger
//! reading of the same state, in the place a player is actually looking while their
//! character is exposed -- it does not replace the first, and the two are composed
//! separately on purpose: the status readout is a transport debug surface that a release
//! build would stop drawing, and this is not.

use bevy::prelude::*;
use bevy::ui::FocusPolicy;

use super::health::DEFAULT_FONT_ADVANCE_EM;
use crate::net::{ConnectionState, DrainNetwork};

/// This overlay's layer.
///
/// Below every layer `ui/health.rs` owns, and therefore below the death overlay and the
/// pause menu -- which is the requirement, since a player who dies during the linger must
/// still see they died, and the controls that leave a session must never be buried. The
/// ordering is asserted at compile time in `ui/health.rs`, in the same chain that already
/// runs from the vignette up to [`super::menu::MENU_LAYER`], rather than being a number
/// picked to look right beside its neighbours.
///
/// It is above the HUD (12) and the chat log (14): those are readings a player chooses to
/// consult, and this is one they must not be able to miss.
pub(super) const LEAVING_LAYER: i32 = 17;

/// The narrowest window this overlay's text is required to fit across, in logical pixels.
///
/// A floor rather than a measurement -- the client sets no minimum window size, so this is
/// the width below which the assertions under it stop being claims about anything. 800 is
/// the smallest window anybody would play in, and every line below is checked against it at
/// compile time.
const MIN_VIEWPORT_WIDTH: f32 = 800.0;

/// The panel's inside padding, on every side.
const PANEL_PADDING: f32 = 28.0;

/// What a line may span before it would reach the edge of the narrowest window.
const PANEL_INNER_WIDTH: f32 = MIN_VIEWPORT_WIDTH - 2.0 * PANEL_PADDING;

const TITLE_SIZE: f32 = 30.0;
const COUNTDOWN_SIZE: f32 = 64.0;
const DETAIL_SIZE: f32 = 18.0;

/// The heading. Fixed, because the thing that is happening does not change while it
/// happens; only the number and the reason under it do.
const TITLE: &str = "LEAVING THE WORLD";

/// The countdown, once the server has named one. `{n}s`.
///
/// The longest reading is the wire's maximum rather than the linger the server actually
/// uses: `seconds_remaining` is a `u32`, so `4294967295s` is eleven characters and the fit
/// below is a bound over every value that can reach this module, not over the ten seconds
/// anybody expects.
const LONGEST_COUNTDOWN_CHARS: f32 = 11.0;

/// What stands where the number goes before the server has named one.
///
/// **Deliberately not a digit.** A `10` here would be this client asserting a duration it
/// has not been told, in the one place the schema says it may not; a `0` would be worse,
/// because zero is a value the countdown genuinely passes through. Two dashes are visibly
/// a placeholder, and the line under them says what is being waited for.
const NO_COUNTDOWN_YET: &str = "--";

/// The sentence under the number once there is one.
const STILL_IN_THE_WORLD: &str = "YOUR CHARACTER IS STILL IN THE WORLD";

/// And what it says while the acknowledgement is outstanding.
const WAITING_FOR_THE_SERVER: &str =
    "WAITING FOR THE SERVER - YOUR CHARACTER IS STILL IN THE WORLD";

/// Every line fits across the narrowest window, at compile time.
///
/// The readings are laid out `TextLayout::no_wrap`, so a line that outgrew the panel would
/// not wrap into it -- it would spill past the window with nothing at runtime saying so.
/// Bevy's embedded `FiraMono` is monospace, so [`DEFAULT_FONT_ADVANCE_EM`] makes each of
/// these an exact bound rather than an estimate. A longer sentence, or a larger size, fails
/// the build.
const _: () = assert!(
    TITLE.len() as f32 * DEFAULT_FONT_ADVANCE_EM * TITLE_SIZE <= PANEL_INNER_WIDTH,
    "the heading must fit across the narrowest window - shorten TITLE or lower TITLE_SIZE"
);
const _: () = assert!(
    LONGEST_COUNTDOWN_CHARS * DEFAULT_FONT_ADVANCE_EM * COUNTDOWN_SIZE <= PANEL_INNER_WIDTH,
    "the longest wire-valid countdown must fit - lower COUNTDOWN_SIZE"
);
const _: () = assert!(
    WAITING_FOR_THE_SERVER.len() as f32 * DEFAULT_FONT_ADVANCE_EM * DETAIL_SIZE
        <= PANEL_INNER_WIDTH,
    "the longer of the two sentences must fit - shorten it or lower DETAIL_SIZE"
);
const _: () = assert!(
    STILL_IN_THE_WORLD.len() <= WAITING_FOR_THE_SERVER.len(),
    "the assertion above is only a bound on both sentences while this one is the shorter"
);

/// The panel behind the text. Dark enough that the reading survives any sky, and short of
/// opaque so the world the character is still standing in remains visible through it.
const PANEL: Color = Color::srgba(0.035, 0.042, 0.055, 0.88);

/// The heading and the sentence.
const LABEL: Color = Color::srgb(0.86, 0.88, 0.92);

/// The number itself.
///
/// Amber, and specifically neither of the two colours the HUD already spends: `ui/health.rs`
/// reserves red for harm and ice for the server refusing it. A leave in progress is neither
/// -- it is a clock running out on a character who is still exactly as killable as before --
/// so it gets the one warm tone nothing else here uses.
const COUNTDOWN: Color = Color::srgb(0.95, 0.76, 0.32);

pub(super) struct LeavingUiPlugin;

impl Plugin for LeavingUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_leave_overlay).add_systems(
            Update,
            (show_leave_overlay, refresh_leave_overlay)
                // After the events that move the connection, so entering and leaving
                // `Leaving` both reach the screen on the frame the net thread reports
                // them rather than the one after. Ordering against a set no plugin has
                // registered is a no-op, which is what keeps this module drivable on a
                // headless app with no `NetPlugin` built at all.
                .after(DrainNetwork),
        );
    }
}

/// The overlay's root. Hidden and shown as one node.
#[derive(Component)]
struct LeaveRoot;

/// The line the remaining whole seconds are written into.
#[derive(Component)]
struct LeaveCountdownText;

/// The line that says what is still true of the character.
#[derive(Component)]
struct LeaveDetailText;

fn spawn_leave_overlay(mut commands: Commands) {
    commands
        .spawn((
            LeaveRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            // **The pointer passes straight through, and so does everything else.** This
            // is a reading, not a control: there is nothing on it to click, the leave it
            // reports cannot be cancelled, and a node that blocked would take the pointer
            // away from the surfaces that can still be used. It is also why this overlay
            // is not one of `ui/mod.rs`'s `Overlays` -- those are the screens whose being
            // up means this frame's input is not for the world, and this one changes
            // nothing about where input goes.
            FocusPolicy::Pass,
            Visibility::Hidden,
            GlobalZIndex(LEAVING_LAYER),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        row_gap: Val::Px(6.0),
                        padding: UiRect::all(Val::Px(PANEL_PADDING)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(PANEL),
                    FocusPolicy::Pass,
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new(TITLE),
                        TextFont {
                            font_size: FontSize::Px(TITLE_SIZE),
                            ..default()
                        },
                        TextColor(LABEL),
                        TextLayout::no_wrap().with_justify(Justify::Center),
                        TextShadow::default(),
                    ));
                    panel.spawn((
                        LeaveCountdownText,
                        // The waiting form, because that is the state a leave begins in
                        // and the only one this client can know without being told.
                        Text::new(NO_COUNTDOWN_YET),
                        TextFont {
                            font_size: FontSize::Px(COUNTDOWN_SIZE),
                            ..default()
                        },
                        TextColor(COUNTDOWN),
                        TextLayout::no_wrap().with_justify(Justify::Center),
                        TextShadow::default(),
                    ));
                    panel.spawn((
                        LeaveDetailText,
                        Text::new(WAITING_FOR_THE_SERVER),
                        TextFont {
                            font_size: FontSize::Px(DETAIL_SIZE),
                            ..default()
                        },
                        TextColor(LABEL),
                        TextLayout::no_wrap().with_justify(Justify::Center),
                        TextShadow::default(),
                    ));
                });
        });
}

/// Shows the overlay exactly while the connection is in [`ConnectionState::Leaving`].
///
/// Unconditional rather than change-detected, and that is the half that answers "the
/// overlay disappears on any exit". Every way a leave ends -- the socket closing, a
/// refusal, a disconnection, the resource being removed outright -- is some state that is
/// not `Leaving`, and none of them is a number reaching zero.
fn show_leave_overlay(
    state: Option<Res<ConnectionState>>,
    mut roots: Query<&mut Visibility, With<LeaveRoot>>,
) {
    let next = if leaving(state.as_deref()) {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut roots {
        if *visibility != next {
            *visibility = next;
        }
    }
}

/// Writes the two lines that depend on what the server has said.
///
/// **No clock appears among these parameters, and that is the point.** Local time may pass
/// for as long as it likes; the strings are not rebuilt, because the only thing they render
/// has not moved. The number changes when `net/` publishes a new [`ConnectionState`] and at
/// no other moment.
fn refresh_leave_overlay(
    state: Option<Res<ConnectionState>>,
    mut countdowns: Query<&mut Text, (With<LeaveCountdownText>, Without<LeaveDetailText>)>,
    mut details: Query<&mut Text, (With<LeaveDetailText>, Without<LeaveCountdownText>)>,
) {
    let Some(state) = state else {
        return;
    };
    if !state.is_changed() {
        return;
    }
    let ConnectionState::Leaving { seconds_remaining } = *state else {
        // Left exactly as it was. The overlay is hidden, and rewriting lines nobody can
        // read would only spend the allocation this guard exists to avoid.
        return;
    };

    let countdown = countdown_line(seconds_remaining);
    for mut text in &mut countdowns {
        if text.0 != countdown {
            text.0.clone_from(&countdown);
        }
    }

    let detail = detail_line(seconds_remaining);
    for mut text in &mut details {
        if text.0 != detail {
            text.0.clear();
            text.0.push_str(detail);
        }
    }
}

/// Whether the connection is mid-leave. Absent is not leaving: a client with no
/// connection resource at all has nothing to count down.
fn leaving(state: Option<&ConnectionState>) -> bool {
    matches!(state, Some(ConnectionState::Leaving { .. }))
}

/// The server's remaining whole seconds, or the placeholder that is not a number.
fn countdown_line(seconds_remaining: Option<u32>) -> String {
    match seconds_remaining {
        Some(seconds) => format!("{seconds}s"),
        None => NO_COUNTDOWN_YET.to_owned(),
    }
}

/// What is still true of the character, and -- until the acknowledgement arrives -- what
/// the client is waiting for.
fn detail_line(seconds_remaining: Option<u32>) -> &'static str {
    match seconds_remaining {
        Some(_) => STILL_IN_THE_WORLD,
        None => WAITING_FOR_THE_SERVER,
    }
}

#[cfg(test)]
mod tests {
    //! No window, no display and no GPU -- `MinimalPlugins` and this plugin are the whole
    //! app.
    //!
    //! **#536 is why these assertions do not stop at `Visibility`.** A full-screen overlay
    //! there was implemented, component-tested and reported as never drawn; what the
    //! measurement eventually showed was that the component state had been the only thing
    //! anybody had ever checked, so every test agreed with the code and none of them knew
    //! whether a pixel moved. The answer taken there was to compute the render path's
    //! *output* rather than assert its input, and the same idea reaches a text overlay
    //! through its layout: the three `const _: () = assert!` above resolve the widest line
    //! each reading can be asked to draw against the narrowest window it may be drawn in,
    //! using the monospace advance of the only font this client has, so a heading that
    //! outgrew the panel fails the build rather than silently running off the screen.
    //!
    //! **What this harness still cannot reach, stated rather than implied**: no renderer
    //! runs headless and CI's `client` job has no GPU, so nothing below proves a pixel
    //! changed colour. What it does prove is that exactly one state puts the node up, that
    //! every other state takes it down, that the number is the server's, and that no
    //! passage of local time moves it.

    use super::super::health::DEATH_LAYER;
    use super::super::menu::MENU_LAYER;
    use super::*;

    /// This plugin on a headless app, in a given connection state.
    fn overlay(state: ConnectionState) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(state)
            .add_plugins(LeavingUiPlugin);
        app.update();
        app
    }

    /// Replaces the resource exactly as `net/` does when the connection moves.
    fn deliver(app: &mut App, state: ConnectionState) {
        app.insert_resource(state);
        app.update();
    }

    fn visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<LeaveRoot>>();
        *query.single(world).expect("one leave overlay root")
    }

    fn countdown(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<LeaveCountdownText>>();
        query.single(world).expect("one countdown line").0.clone()
    }

    fn detail(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<LeaveDetailText>>();
        query.single(world).expect("one detail line").0.clone()
    }

    fn leaving_in(seconds: u32) -> ConnectionState {
        ConnectionState::Leaving {
            seconds_remaining: Some(seconds),
        }
    }

    const WAITING: ConnectionState = ConnectionState::Leaving {
        seconds_remaining: None,
    };

    // ---------------------------------------------------------------------------
    // What is on screen
    // ---------------------------------------------------------------------------

    #[test]
    fn the_servers_seconds_are_what_the_overlay_says() {
        let mut app = overlay(leaving_in(10));
        assert_eq!(visibility(&mut app), Visibility::Visible);
        assert_eq!(countdown(&mut app), "10s");
        assert_eq!(detail(&mut app), STILL_IN_THE_WORLD);

        // Every later value is the one the server published, including the zero the
        // countdown genuinely passes through on its way to a socket closure.
        for seconds in [9, 5, 1, 0] {
            deliver(&mut app, leaving_in(seconds));
            assert_eq!(countdown(&mut app), format!("{seconds}s"));
            assert_eq!(
                visibility(&mut app),
                Visibility::Visible,
                "zero is a number, not a completion: only the socket closing ends a leave"
            );
        }

        // The wire's maximum, which is what the compile-time fit above is a bound over.
        deliver(&mut app, leaving_in(u32::MAX));
        assert_eq!(countdown(&mut app), "4294967295s");
    }

    #[test]
    fn the_waiting_form_invents_no_number() {
        let mut app = overlay(WAITING);
        assert_eq!(visibility(&mut app), Visibility::Visible);
        assert_eq!(detail(&mut app), WAITING_FOR_THE_SERVER);

        // The whole overlay, not merely the line that would have held one: the interval
        // before the acknowledgement is the state a client is most tempted to fill in.
        for line in [TITLE.to_owned(), countdown(&mut app), detail(&mut app)] {
            assert!(
                !line.chars().any(|character| character.is_ascii_digit()),
                "the waiting form named a duration nobody has sent this client: {line}"
            );
        }
    }

    /// The acknowledgement replaces the placeholder, and going back to waiting -- a second
    /// session, leaving again -- restores it rather than leaving the last number up.
    #[test]
    fn the_acknowledgement_replaces_the_placeholder_and_the_placeholder_comes_back() {
        let mut app = overlay(WAITING);
        assert_eq!(countdown(&mut app), NO_COUNTDOWN_YET);

        deliver(&mut app, leaving_in(10));
        assert_eq!(countdown(&mut app), "10s");
        assert_eq!(detail(&mut app), STILL_IN_THE_WORLD);

        deliver(&mut app, ConnectionState::Disconnected);
        deliver(&mut app, ConnectionState::Connected);
        deliver(&mut app, WAITING);
        assert_eq!(countdown(&mut app), NO_COUNTDOWN_YET);
        assert_eq!(detail(&mut app), WAITING_FOR_THE_SERVER);
    }

    // ---------------------------------------------------------------------------
    // When it is on screen
    // ---------------------------------------------------------------------------

    /// Driven off [`ConnectionState::every`] rather than a list of its own, for the reason
    /// that list documents: a state added to the enum and forgotten by a sweep leaves the
    /// sweep reading as exhaustive while covering less than it did.
    #[test]
    fn exactly_the_two_leaving_states_put_the_overlay_up() {
        for state in ConnectionState::every() {
            let expected = match state {
                ConnectionState::Leaving { .. } => Visibility::Visible,
                ConnectionState::Idle
                | ConnectionState::Connecting
                | ConnectionState::Handshaking
                | ConnectionState::Choosing
                | ConnectionState::Connected
                | ConnectionState::Rejected { .. }
                | ConnectionState::Disconnected => Visibility::Hidden,
            };
            let mut app = overlay(state.clone());
            assert_eq!(visibility(&mut app), expected, "in {state:?}");
        }
    }

    /// A client with no connection resource at all -- the shape `ui/servers.rs` already
    /// reads the state in -- has nothing to count down and draws nothing.
    #[test]
    fn no_connection_resource_is_not_a_leave() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(LeavingUiPlugin);
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Hidden);
    }

    /// **The socket closing is the completion, not the number.** A leave that ends while
    /// the count is still high takes the overlay with it on the frame the state moves.
    #[test]
    fn the_overlay_goes_away_on_a_closure_that_beats_the_countdown() {
        let mut app = overlay(leaving_in(7));
        assert_eq!(visibility(&mut app), Visibility::Visible);
        assert_eq!(countdown(&mut app), "7s");

        deliver(&mut app, ConnectionState::Disconnected);
        assert_eq!(visibility(&mut app), Visibility::Hidden);

        // Every other exit is the same exit. A refusal and a rejoin are not special cases
        // here; they are simply states that are not `Leaving`.
        for state in [
            ConnectionState::Rejected {
                reason: "refused".to_owned(),
            },
            ConnectionState::Idle,
            ConnectionState::Connected,
        ] {
            deliver(&mut app, leaving_in(7));
            assert_eq!(visibility(&mut app), Visibility::Visible);
            deliver(&mut app, state);
            assert_eq!(visibility(&mut app), Visibility::Hidden);
        }
    }

    /// Removing the resource outright is an exit too, and it carries no change signal for
    /// the text system to read -- which is exactly why visibility is decided unconditionally.
    #[test]
    fn removing_the_connection_resource_takes_the_overlay_down() {
        let mut app = overlay(leaving_in(4));
        assert_eq!(visibility(&mut app), Visibility::Visible);

        app.world_mut().remove_resource::<ConnectionState>();
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Hidden);
    }

    // ---------------------------------------------------------------------------
    // Whose clock it is
    // ---------------------------------------------------------------------------

    /// **The client never counts down on its own clock.** Two hundred frames pass with no
    /// new state, and the reading is the last one the server sent -- not one lower, and not
    /// a zero. `net/` owns the presentation clock behind `seconds_remaining`; this module
    /// owns none.
    #[test]
    fn local_frames_do_not_move_the_servers_number() {
        let mut app = overlay(leaving_in(10));
        assert_eq!(countdown(&mut app), "10s");

        for _ in 0..200 {
            app.update();
        }

        assert_eq!(countdown(&mut app), "10s");
        assert_eq!(detail(&mut app), STILL_IN_THE_WORLD);
        assert_eq!(visibility(&mut app), Visibility::Visible);
    }

    // ---------------------------------------------------------------------------
    // Where it sits
    // ---------------------------------------------------------------------------

    /// The layer requirement, read off the constants the nodes are actually spawned with.
    /// The full ordering is asserted at compile time in `ui/health.rs`; this is the half
    /// of it this issue names, stated where somebody reading this module will find it.
    #[test]
    fn the_overlay_sits_under_the_death_overlay_and_the_menu() {
        const { assert!(LEAVING_LAYER < DEATH_LAYER) };
        const { assert!(LEAVING_LAYER < MENU_LAYER) };

        let mut app = overlay(leaving_in(3));
        let world = app.world_mut();
        let mut query = world.query_filtered::<&GlobalZIndex, With<LeaveRoot>>();
        assert_eq!(
            query.single(world).expect("one leave overlay root").0,
            LEAVING_LAYER
        );
    }

    /// It spans the window and passes the pointer through: a reading with nothing on it to
    /// click must not take input away from the surfaces that have.
    #[test]
    fn the_overlay_spans_the_window_and_captures_nothing() {
        let mut app = overlay(leaving_in(3));
        let world = app.world_mut();

        let mut roots = world.query_filtered::<(&Node, &FocusPolicy), With<LeaveRoot>>();
        let (node, policy) = roots.single(world).expect("one leave overlay root");
        assert_eq!(node.position_type, PositionType::Absolute);
        assert_eq!(node.width, Val::Percent(100.0));
        assert_eq!(node.height, Val::Percent(100.0));
        assert_eq!(node.align_items, AlignItems::Center);
        assert_eq!(node.justify_content, JustifyContent::Center);
        assert_eq!(*policy, FocusPolicy::Pass);

        // Nothing anywhere under the root blocks either, since a child that did would
        // capture the pointer the root deliberately passes on.
        let mut policies = world.query::<&FocusPolicy>();
        assert!(
            policies
                .iter(world)
                .all(|policy| *policy == FocusPolicy::Pass),
            "a node in this overlay blocks the pointer"
        );
    }

    /// The layout bound the `const` assertions above rest on, restated as a runtime check
    /// so a reader can see the numbers rather than only the failure.
    #[test]
    fn every_line_fits_across_the_narrowest_window() {
        let widest =
            |characters: usize, size: f32| characters as f32 * DEFAULT_FONT_ADVANCE_EM * size;
        assert!(widest(TITLE.len(), TITLE_SIZE) <= PANEL_INNER_WIDTH);
        assert!(widest("4294967295s".len(), COUNTDOWN_SIZE) <= PANEL_INNER_WIDTH);
        assert!(widest(WAITING_FOR_THE_SERVER.len(), DETAIL_SIZE) <= PANEL_INNER_WIDTH);
        assert!(widest(STILL_IN_THE_WORLD.len(), DETAIL_SIZE) <= PANEL_INNER_WIDTH);
        assert!(widest(NO_COUNTDOWN_YET.len(), COUNTDOWN_SIZE) <= PANEL_INNER_WIDTH);

        // The countdown the server actually sends, which is what a player sees: eleven
        // characters is the wire bound, three is the reading.
        assert_eq!(countdown_line(Some(10)), "10s");
        assert_eq!(countdown_line(None), NO_COUNTDOWN_YET);
    }

    /// No reading here is laid out with a wrap, so the fit above is the whole of what
    /// keeps a line on the screen.
    #[test]
    fn no_reading_wraps() {
        let mut app = overlay(leaving_in(3));
        let world = app.world_mut();
        let mut query = world.query::<&TextLayout>();
        let layouts: Vec<TextLayout> = query.iter(world).copied().collect();
        assert_eq!(layouts.len(), 3, "the overlay draws exactly three readings");
        for layout in layouts {
            assert_eq!(layout.linebreak, LineBreak::NoWrap);
            assert_eq!(layout.justify, Justify::Center);
        }
    }
}
