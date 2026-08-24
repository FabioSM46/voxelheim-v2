//! The hunger bar and its low-reserve reminder.
//!
//! Hunger itself is entirely authoritative: the fill and label are the newest
//! [`PlayerVitals`] the server sent through [`SelfVitals`]. This module never drains,
//! restores or predicts it. The only local time here is presentation — a short colour
//! pulse when that server-sent ratio enters the low range, repeated every ten minutes
//! while it stays there. Unlike the respawn count in `ui/health.rs`, that reminder does
//! not claim when a gameplay event happens, so a Bevy [`Timer`] is the right clock.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::Real;

use super::CELL_EDGE;
use super::health::{BAR_BORDER, BAR_HEIGHT, BAR_WIDTH, HUNGER_BAR_BOTTOM};
use crate::net::{PlayerVitals, Session};
use crate::player::{ApplySnapshots, InputMode, SelfVitals};

/// The ratio below which the presentation reminder is active. Exactly 25% is not low.
const LOW_HUNGER_PERCENT: u32 = 25;

/// Time between reminder bursts while the authoritative reserve remains low.
const REMINDER_PERIOD: Duration = Duration::from_secs(10 * 60);

/// Length of one reminder burst.
const BLINK_BURST_DURATION: Duration = Duration::from_secs(2);

/// Full bright-to-rest pulses inside one burst.
const BLINK_PULSES: u32 = 4;

/// The empty part of the bar, shared visually with the health and inventory tracks.
const BAR_TRACK: Color = Color::srgba(0.055, 0.065, 0.080, 0.94);

/// The ordinary hunger fill.
const BAR_FILL: Color = Color::srgb(0.78, 0.55, 0.10);

/// The bright half of each low-hunger pulse.
const LOW_HUNGER_FLASH: Color = Color::srgb(1.0, 0.88, 0.34);

pub(super) struct HungerUiPlugin;

impl Plugin for HungerUiPlugin {
    fn build(&self, app: &mut App) {
        // The player plugin owns both resources in the game. Initialising them here keeps
        // this module independently testable with `MinimalPlugins`.
        app.init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<LowHungerReminder>()
            .add_systems(Startup, spawn_hunger_bar)
            .add_systems(
                Update,
                (refresh_hunger_bar, show_hunger_bar, drive_low_hunger_blink).after(ApplySnapshots),
            );
    }
}

/// The bar and everything inside it. Hidden and shown as one node.
#[derive(Component)]
struct HungerRoot;

/// The filled part. Its width is the server's ratio and its colour carries the reminder.
#[derive(Component)]
struct HungerFill;

/// The numeric reading beside the bar.
#[derive(Component)]
struct HungerLabel;

/// Presentation-only state for the low-hunger cycle.
///
/// `was_low` is the edge detector. The two timers hold separate questions: whether a
/// burst is currently visible, and when the next burst is due. Neither touches gameplay
/// state or feeds an input decision.
#[derive(Resource)]
struct LowHungerReminder {
    was_low: bool,
    burst: Timer,
    reminder: Timer,
}

impl Default for LowHungerReminder {
    fn default() -> Self {
        Self {
            was_low: false,
            burst: Timer::new(BLINK_BURST_DURATION, TimerMode::Once),
            reminder: Timer::new(REMINDER_PERIOD, TimerMode::Repeating),
        }
    }
}

impl LowHungerReminder {
    fn enter_low(&mut self) {
        self.was_low = true;
        self.burst.reset();
        self.reminder.reset();
    }

    fn leave_low(&mut self) {
        self.was_low = false;
        self.burst.reset();
        self.reminder.reset();
    }
}

fn spawn_hunger_bar(mut commands: Commands) {
    commands
        .spawn((
            HungerRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(HUNGER_BAR_BOTTOM),
                display: Display::Flex,
                column_gap: Val::Px(10.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            Visibility::Hidden,
            GlobalZIndex(12),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    width: Val::Px(BAR_WIDTH),
                    height: Val::Px(BAR_HEIGHT),
                    border: UiRect::all(Val::Px(BAR_BORDER)),
                    border_radius: BorderRadius::all(Val::Px(3.0)),
                    ..default()
                },
                BackgroundColor(BAR_TRACK),
                BorderColor::all(CELL_EDGE),
            ))
            .with_child((
                HungerFill,
                Node {
                    // Zero until a server snapshot supplies a ratio.
                    width: Val::Percent(0.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(BAR_FILL),
            ));

            root.spawn((
                HungerLabel,
                Text::new(String::new()),
                TextFont {
                    font_size: FontSize::Px(17.0),
                    ..default()
                },
                TextColor(Color::WHITE),
                TextShadow::default(),
            ));
        });
}

/// Draws the newest authoritative hunger ratio.
///
/// The time-driven colour is deliberately absent from this system. The change flag is
/// appropriate for the server-sent number and label, but cannot drive an animation while
/// those values stay unchanged.
fn refresh_hunger_bar(
    vitals: Res<SelfVitals>,
    mut fills: Query<&mut Node, With<HungerFill>>,
    mut labels: Query<&mut Text, With<HungerLabel>>,
) {
    if !vitals.is_changed() {
        return;
    }
    let Some(current) = vitals.get() else {
        return;
    };

    let width = Val::Percent(fill_percent(current));
    for mut node in &mut fills {
        if node.width != width {
            node.width = width;
        }
    }

    let label = format!("{} / {}", current.hunger, current.max_hunger);
    for mut text in &mut labels {
        if text.0 != label {
            text.0.clone_from(&label);
        }
    }
}

/// Shows under exactly the health bar's conditions: playing, connected, and told vitals.
fn show_hunger_bar(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut roots: Query<&mut Visibility, With<HungerRoot>>,
) {
    let next = if *mode == InputMode::Playing && session.is_some() && vitals.get().is_some() {
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

/// Advances only the presentation reminder, once per frame.
///
/// A low value arriving for the first time starts bright immediately and resets the
/// ten-minute period. Returning to 25% or above cancels both timers; a later low value is
/// therefore a fresh crossing rather than the tail of an older cycle. [`Time<Real>`]
/// keeps those ten minutes on the wall clock instead of stretching them with a paused or
/// scaled game clock.
fn drive_low_hunger_blink(
    time: Res<Time<Real>>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut state: ResMut<LowHungerReminder>,
    mut fills: Query<&mut BackgroundColor, With<HungerFill>>,
) {
    let low = session.is_some() && vitals.get().is_some_and(is_low);
    let colour = if !low {
        if state.was_low {
            state.leave_low();
        }
        BAR_FILL
    } else if !state.was_low {
        state.enter_low();
        LOW_HUNGER_FLASH
    } else {
        state.reminder.tick(time.delta());
        if state.reminder.just_finished() {
            // A large frame may cross more than one period, but only one current burst
            // can be drawn. `TimerMode::Repeating` preserves the remainder for the next.
            state.burst.reset();
        } else {
            state.burst.tick(time.delta());
        }
        burst_colour(&state.burst)
    };

    let next = BackgroundColor(colour);
    for mut background in &mut fills {
        if *background != next {
            *background = next;
        }
    }
}

/// How much of the bar the server's hunger fills, as a percentage of its width.
fn fill_percent(vitals: PlayerVitals) -> f32 {
    // The decoder guarantees a non-zero maximum and `hunger <= max_hunger`. The clamp is
    // defensive presentation, as it is for health: an overflowing fill looks merely full.
    (f32::from(vitals.hunger) * 100.0 / f32::from(vitals.max_hunger)).clamp(0.0, 100.0)
}

/// Whether the authoritative reserve is strictly below the named percentage.
fn is_low(vitals: PlayerVitals) -> bool {
    u32::from(vitals.hunger) * 100 < u32::from(vitals.max_hunger) * LOW_HUNGER_PERCENT
}

/// The colour for this instant of an active burst.
fn burst_colour(burst: &Timer) -> Color {
    if burst.is_finished() {
        return BAR_FILL;
    }

    let half_pulse = (burst.fraction() * (BLINK_PULSES * 2) as f32).floor() as u32;
    if half_pulse.is_multiple_of(2) {
        LOW_HUNGER_FLASH
    } else {
        BAR_FILL
    }
}

#[cfg(test)]
mod tests {
    //! Headless assertions against the exact nodes, colours and timer transitions.

    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{LifeState, SessionParams};
    use crate::ui::health::{HEALTH_BAR_BOTTOM, VITAL_BAR_GAP};

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 36,
            hotbar_slots: 9,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn vitals(hunger: u16, max_hunger: u16) -> PlayerVitals {
        PlayerVitals {
            health: 100,
            max_health: 100,
            hunger,
            max_hunger,
            life_state: LifeState::Alive,
            respawn_ticks: 0,
            invulnerable: false,
        }
    }

    fn hud(first: Option<PlayerVitals>, step: Duration) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(step))
            .insert_resource(session());
        if let Some(first) = first {
            app.insert_resource(SelfVitals::from_server(first));
        }
        app.add_plugins(HungerUiPlugin);
        app.update();
        app
    }

    fn deliver(app: &mut App, next: PlayerVitals) {
        app.insert_resource(SelfVitals::from_server(next));
        app.update();
    }

    fn advance(app: &mut App, frames: usize) {
        for _ in 0..frames {
            app.update();
        }
    }

    fn fill_width(app: &mut App) -> Val {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<HungerFill>>();
        query.single(world).expect("one hunger fill").width
    }

    fn fill_colour(app: &mut App) -> Color {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&BackgroundColor, With<HungerFill>>();
        query.single(world).expect("one hunger fill").0
    }

    fn label(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<HungerLabel>>();
        query.single(world).expect("one hunger label").0.clone()
    }

    fn visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<HungerRoot>>();
        *query.single(world).expect("one hunger root")
    }

    #[test]
    fn the_fill_is_exactly_the_servers_hunger_over_its_maximum() {
        assert_eq!(fill_percent(vitals(100, 100)), 100.0);
        assert_eq!(fill_percent(vitals(0, 100)), 0.0);
        assert_eq!(fill_percent(vitals(3, 12)), 25.0);
        assert_eq!(fill_percent(vitals(1, 1)), 100.0);
        assert_eq!(fill_percent(vitals(u16::MAX, u16::MAX)), 100.0);

        let mut app = hud(Some(vitals(7, 20)), Duration::from_millis(100));
        assert_eq!(fill_width(&mut app), Val::Percent(35.0));
        assert_eq!(label(&mut app), "7 / 20");

        deliver(&mut app, vitals(0, 20));
        assert_eq!(fill_width(&mut app), Val::Percent(0.0));
        assert_eq!(label(&mut app), "0 / 20");
    }

    #[test]
    fn both_vital_bars_have_explicit_non_overlapping_positions() {
        assert_eq!(
            HEALTH_BAR_BOTTOM,
            HUNGER_BAR_BOTTOM + BAR_HEIGHT + VITAL_BAR_GAP
        );

        let mut app = hud(Some(vitals(100, 100)), Duration::from_millis(100));
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<HungerRoot>>();
        assert_eq!(
            query.single(world).expect("one hunger root").bottom,
            Val::Px(HUNGER_BAR_BOTTOM)
        );
    }

    #[test]
    fn visibility_matches_the_health_bar_conditions() {
        let mut app = hud(None, Duration::from_millis(100));
        assert_eq!(visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, vitals(80, 100));
        assert_eq!(visibility(&mut app), Visibility::Visible);

        for mode in [InputMode::Inventory, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(visibility(&mut app), Visibility::Hidden, "mode {mode:?}");
        }

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.world_mut().remove_resource::<Session>();
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn low_is_strictly_below_twenty_five_percent() {
        assert!(is_low(vitals(24, 100)));
        assert!(is_low(vitals(1, 5)));
        assert!(!is_low(vitals(25, 100)));
        assert!(!is_low(vitals(1, 4)));
        assert!(!is_low(vitals(26, 100)));
    }

    #[test]
    fn crossing_low_starts_a_brief_multi_pulse_burst_immediately() {
        let mut app = hud(Some(vitals(25, 100)), Duration::from_millis(250));
        assert_eq!(fill_colour(&mut app), BAR_FILL);

        deliver(&mut app, vitals(24, 100));
        assert_eq!(fill_colour(&mut app), LOW_HUNGER_FLASH);

        let mut colours = Vec::new();
        for _ in 0..7 {
            app.update();
            colours.push(fill_colour(&mut app));
        }
        assert_eq!(
            colours,
            vec![
                BAR_FILL,
                LOW_HUNGER_FLASH,
                BAR_FILL,
                LOW_HUNGER_FLASH,
                BAR_FILL,
                LOW_HUNGER_FLASH,
                BAR_FILL,
            ]
        );

        // The eighth quarter-second finishes the two-second burst and leaves the fill at
        // rest until the reminder period expires.
        app.update();
        assert_eq!(fill_colour(&mut app), BAR_FILL);
        advance(&mut app, 20);
        assert_eq!(fill_colour(&mut app), BAR_FILL);
    }

    #[test]
    fn a_low_reserve_repeats_one_burst_after_ten_minutes() {
        let mut app = hud(Some(vitals(24, 100)), Duration::from_secs(1));
        assert_eq!(fill_colour(&mut app), LOW_HUNGER_FLASH);

        // One frame before the period, the initial burst is long over and nothing blinks.
        advance(&mut app, 599);
        assert_eq!(fill_colour(&mut app), BAR_FILL);

        app.update();
        assert_eq!(fill_colour(&mut app), LOW_HUNGER_FLASH);
        advance(&mut app, 2);
        assert_eq!(fill_colour(&mut app), BAR_FILL);
    }

    #[test]
    fn eating_to_the_threshold_cancels_the_pending_reminder() {
        let mut app = hud(Some(vitals(24, 100)), Duration::from_secs(60));
        assert_eq!(fill_colour(&mut app), LOW_HUNGER_FLASH);

        advance(&mut app, 5);
        deliver(&mut app, vitals(25, 100));
        assert_eq!(fill_colour(&mut app), BAR_FILL);

        // More than the old cycle's remaining ten minutes cannot revive it.
        advance(&mut app, 11);
        assert_eq!(fill_colour(&mut app), BAR_FILL);
    }

    #[test]
    fn reentering_low_restarts_the_immediate_burst_and_full_period() {
        let mut app = hud(Some(vitals(24, 100)), Duration::from_secs(60));
        advance(&mut app, 5);

        deliver(&mut app, vitals(30, 100));
        assert_eq!(fill_colour(&mut app), BAR_FILL);
        advance(&mut app, 2);

        deliver(&mut app, vitals(24, 100));
        assert_eq!(fill_colour(&mut app), LOW_HUNGER_FLASH);
        advance(&mut app, 9);
        assert_eq!(fill_colour(&mut app), BAR_FILL);

        app.update();
        assert_eq!(fill_colour(&mut app), LOW_HUNGER_FLASH);
    }
}
