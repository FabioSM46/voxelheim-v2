//! The authoritative experience bar and its presentation-only level-up flash.
//!
//! The fill, level and fraction are always the newest [`PlayerVitals`] sent by the
//! server. This module never awards experience, computes a curve or advances a level.
//! The only local clock is the brief level-up flash: it describes how long a visual
//! acknowledgement remains on screen, not when simulation state changes. A [`Timer`]
//! driven by wall-clock [`Time<Real>`] is therefore appropriate here, unlike the
//! authoritative respawn count in `ui/health.rs`.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::Real;

use super::CELL_EDGE;
use super::health::{
    BAR_LABEL_SIZE, EXPERIENCE_BAR_BOTTOM, vital_bar_label, vital_bar_label_transform,
    vital_bar_root, vital_bar_track,
};
use crate::net::{PlayerVitals, Session};
use crate::player::{ApplySnapshots, InputMode, SelfVitals};

/// How long a newly observed authoritative level remains announced.
const LEVEL_UP_FLASH_DURATION: Duration = Duration::from_secs(2);

/// The empty portion of the experience bar.
const BAR_TRACK: Color = Color::srgba(0.055, 0.065, 0.080, 0.94);

/// The experience fill, distinct from health and hunger without changing either.
const BAR_FILL: Color = Color::srgb(0.36, 0.48, 0.92);

/// The level-up announcement at full opacity.
const LEVEL_UP_FLASH: Color = Color::srgb(0.82, 0.88, 1.0);

const LEVEL_UP_LABEL_SIZE: f32 = 38.0;
const EMPTY_PERCENT: f32 = 0.0;
const FULL_PERCENT: f32 = 100.0;
const OPAQUE_ALPHA: f32 = 1.0;
const HUD_LAYER: i32 = 12;

pub(super) struct ExperienceUiPlugin;

impl Plugin for ExperienceUiPlugin {
    fn build(&self, app: &mut App) {
        // The player plugin owns the first two resources in the game. Initialising them
        // here keeps this module independently testable with `MinimalPlugins`.
        app.init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<LevelUpFlash>()
            .add_systems(Startup, (spawn_experience_bar, spawn_level_up_flash))
            .add_systems(
                Update,
                (
                    refresh_experience_bar,
                    show_experience_bar,
                    (detect_level_up, drive_level_up_flash).chain(),
                )
                    .after(ApplySnapshots),
            );
    }
}

/// The experience bar and everything beside it. Hidden and shown as one node.
#[derive(Component)]
pub(super) struct ExperienceRoot;

#[derive(Component)]
pub(super) struct ExperienceTrack;

/// The filled portion. Its width is the server-sent progression ratio.
#[derive(Component)]
struct ExperienceFill;

/// The level and numeric progression inside the bar.
#[derive(Component)]
pub(super) struct ExperienceLabel;

/// The centred level-up announcement's root.
#[derive(Component)]
struct LevelUpRoot;

/// The text whose opacity fades over the presentation timer.
#[derive(Component)]
struct LevelUpLabel;

/// Presentation state derived from consecutive authoritative levels.
///
/// `last_level` is deliberately absent until the first vitals of a live session arrive:
/// that first value establishes a silent baseline. `fresh` preserves the full configured
/// duration after the frame that detects an increase rather than consuming one frame's
/// delta immediately.
#[derive(Resource)]
struct LevelUpFlash {
    last_level: Option<u16>,
    active_level: Option<u16>,
    timer: Timer,
    fresh: bool,
}

impl Default for LevelUpFlash {
    fn default() -> Self {
        Self {
            last_level: None,
            active_level: None,
            timer: Timer::new(LEVEL_UP_FLASH_DURATION, TimerMode::Once),
            fresh: false,
        }
    }
}

impl LevelUpFlash {
    fn begin(&mut self, level: u16) {
        self.active_level = Some(level);
        self.timer.reset();
        self.fresh = true;
    }

    fn clear_session(&mut self) {
        self.last_level = None;
        self.active_level = None;
        self.timer.reset();
        self.fresh = false;
    }
}

fn spawn_experience_bar(mut commands: Commands) {
    commands
        .spawn((
            ExperienceRoot,
            vital_bar_root(EXPERIENCE_BAR_BOTTOM),
            Visibility::Hidden,
            GlobalZIndex(HUD_LAYER),
        ))
        .with_children(|root| {
            root.spawn((
                ExperienceTrack,
                vital_bar_track(),
                BackgroundColor(BAR_TRACK),
                BorderColor::all(CELL_EDGE),
            ))
            .with_children(|track| {
                track.spawn((
                    ExperienceFill,
                    Node {
                        width: Val::Percent(EMPTY_PERCENT),
                        height: Val::Percent(FULL_PERCENT),
                        ..default()
                    },
                    BackgroundColor(BAR_FILL),
                ));
                track.spawn((
                    ExperienceLabel,
                    vital_bar_label(),
                    vital_bar_label_transform(),
                    Text::new(String::new()),
                    TextFont {
                        font_size: FontSize::Px(BAR_LABEL_SIZE),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                    TextLayout::no_wrap().with_justify(Justify::Center),
                    TextShadow::default(),
                ));
            });
        });
}

fn spawn_level_up_flash(mut commands: Commands) {
    commands
        .spawn((
            LevelUpRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(FULL_PERCENT),
                height: Val::Percent(FULL_PERCENT),
                display: Display::Flex,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            Visibility::Hidden,
            GlobalZIndex(HUD_LAYER),
        ))
        .with_child((
            LevelUpLabel,
            Text::new(String::new()),
            TextFont {
                font_size: FontSize::Px(LEVEL_UP_LABEL_SIZE),
                ..default()
            },
            TextColor(level_up_colour(OPAQUE_ALPHA)),
            TextShadow::default(),
        ));
}

/// Draws the newest authoritative experience ratio and label.
fn refresh_experience_bar(
    vitals: Res<SelfVitals>,
    mut fills: Query<&mut Node, With<ExperienceFill>>,
    mut labels: Query<&mut Text, With<ExperienceLabel>>,
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

    let next = experience_label(current);
    for mut text in &mut labels {
        if text.0 != next {
            text.0.clone_from(&next);
        }
    }
}

/// Shows under exactly the other vital bars' conditions.
fn show_experience_bar(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut roots: Query<&mut Visibility, With<ExperienceRoot>>,
) {
    let next = if matches!(*mode, InputMode::Playing | InputMode::Chat)
        && session.is_some()
        && vitals.get().is_some()
    {
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

/// Establishes a silent baseline, then reacts only to a higher authoritative level.
fn detect_level_up(
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut flash: ResMut<LevelUpFlash>,
) {
    let Some(session) = session else {
        if flash.last_level.is_some() || flash.active_level.is_some() {
            flash.clear_session();
        }
        return;
    };
    // In production the network removes `Session` before another handshake can insert
    // one. Treat replacement as the same boundary as well: it costs nothing and keeps a
    // direct server transfer from comparing the new character with the old baseline.
    if session.is_changed() {
        flash.clear_session();
    }
    if !vitals.is_changed() {
        return;
    }
    let Some(current) = vitals.get() else {
        return;
    };

    let previous = flash.last_level.replace(current.level);
    if previous.is_some_and(|level| current.level > level) {
        flash.begin(current.level);
    }
}

/// Fades the current announcement on real time without feeding it back into gameplay.
fn drive_level_up_flash(
    time: Res<Time<Real>>,
    mut flash: ResMut<LevelUpFlash>,
    mut roots: Query<&mut Visibility, With<LevelUpRoot>>,
    mut labels: Query<(&mut Text, &mut TextColor), With<LevelUpLabel>>,
) {
    let Some(level) = flash.active_level else {
        set_level_up_visibility(&mut roots, Visibility::Hidden);
        return;
    };

    if flash.fresh {
        flash.fresh = false;
    } else {
        flash.timer.tick(time.delta());
    }
    if flash.timer.is_finished() {
        flash.active_level = None;
        set_level_up_visibility(&mut roots, Visibility::Hidden);
        return;
    }

    let label = level_up_label(level);
    let colour = TextColor(level_up_colour(OPAQUE_ALPHA - flash.timer.fraction()));
    for (mut text, mut text_colour) in &mut labels {
        if text.0 != label {
            text.0.clone_from(&label);
        }
        if *text_colour != colour {
            *text_colour = colour;
        }
    }
    set_level_up_visibility(&mut roots, Visibility::Visible);
}

fn set_level_up_visibility(
    roots: &mut Query<&mut Visibility, With<LevelUpRoot>>,
    next: Visibility,
) {
    for mut visibility in roots {
        if *visibility != next {
            *visibility = next;
        }
    }
}

/// How much of the bar the server's experience fills, as a percentage of its width.
fn fill_percent(vitals: PlayerVitals) -> f32 {
    // The decoder guarantees a non-zero denominator and an experience no greater than
    // it. f64 preserves the full u32 ratio before the UI percentage becomes f32.
    ((f64::from(vitals.experience) * f64::from(FULL_PERCENT) / f64::from(vitals.experience_to_next))
        as f32)
        .clamp(EMPTY_PERCENT, FULL_PERCENT)
}

/// The complete progression reading drawn inside the bar.
fn experience_label(vitals: PlayerVitals) -> String {
    format!(
        "Lv {} | {} / {}",
        vitals.level, vitals.experience, vitals.experience_to_next
    )
}

fn level_up_label(level: u16) -> String {
    format!("Level {level}")
}

fn level_up_colour(alpha: f32) -> Color {
    let Srgba {
        red, green, blue, ..
    } = LEVEL_UP_FLASH.to_srgba();
    Color::srgba(red, green, blue, alpha.clamp(EMPTY_PERCENT, OPAQUE_ALPHA))
}

#[cfg(test)]
mod tests {
    //! Headless assertions against the exact nodes and timer transitions.

    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{LifeState, SessionParams};
    use crate::ui::health::{BAR_HEIGHT, HUNGER_BAR_BOTTOM, VITAL_BAR_GAP};

    const STEP: Duration = Duration::from_millis(500);

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn vitals(level: u16, experience: u32, experience_to_next: u32) -> PlayerVitals {
        PlayerVitals {
            health: 100,
            max_health: 100,
            hunger: 100,
            max_hunger: 100,
            level,
            experience,
            experience_to_next,
            life_state: LifeState::Alive,
            respawn_ticks: 0,
            invulnerable: false,
            blocking: false,
        }
    }

    fn hud(first: Option<PlayerVitals>) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
            .insert_resource(session());
        if let Some(first) = first {
            app.insert_resource(SelfVitals::from_server(first));
        }
        app.add_plugins(ExperienceUiPlugin);
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
        let mut query = world.query_filtered::<&Node, With<ExperienceFill>>();
        query.single(world).expect("one experience fill").width
    }

    fn bar_label(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<ExperienceLabel>>();
        query.single(world).expect("one experience label").0.clone()
    }

    fn bar_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<ExperienceRoot>>();
        *query.single(world).expect("one experience root")
    }

    fn flash_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<LevelUpRoot>>();
        *query.single(world).expect("one level-up root")
    }

    fn flash_label(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<LevelUpLabel>>();
        query.single(world).expect("one level-up label").0.clone()
    }

    fn flash_colour(app: &mut App) -> Color {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&TextColor, With<LevelUpLabel>>();
        query.single(world).expect("one level-up label").0
    }

    #[test]
    fn fill_and_label_are_pure_projections_of_the_server_vitals() {
        assert_eq!(fill_percent(vitals(1, 0, 50)), 0.0);
        assert_eq!(fill_percent(vitals(7, 120, 350)), 120.0 / 3.5);
        assert_eq!(fill_percent(vitals(7, 350, 350)), 100.0);
        assert_eq!(fill_percent(vitals(u16::MAX, u32::MAX, u32::MAX)), 100.0);
        assert_eq!(experience_label(vitals(7, 120, 350)), "Lv 7 | 120 / 350");

        let mut app = hud(Some(vitals(7, 120, 350)));
        assert_eq!(fill_width(&mut app), Val::Percent(120.0 / 3.5));
        assert_eq!(bar_label(&mut app), "Lv 7 | 120 / 350");

        deliver(&mut app, vitals(7, 350, 350));
        assert_eq!(fill_width(&mut app), Val::Percent(100.0));
    }

    #[test]
    fn experience_takes_the_lowest_slot_in_the_three_bar_stack() {
        assert_eq!(
            HUNGER_BAR_BOTTOM,
            EXPERIENCE_BAR_BOTTOM + BAR_HEIGHT + VITAL_BAR_GAP
        );

        let mut app = hud(Some(vitals(1, 0, 50)));
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<ExperienceRoot>>();
        assert_eq!(
            query.single(world).expect("one experience root").bottom,
            Val::Px(EXPERIENCE_BAR_BOTTOM)
        );
    }

    #[test]
    fn visibility_matches_the_other_vital_bars() {
        let mut app = hud(None);
        assert_eq!(bar_visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, vitals(1, 0, 50));
        assert_eq!(bar_visibility(&mut app), Visibility::Visible);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        assert_eq!(bar_visibility(&mut app), Visibility::Visible);

        for mode in [InputMode::Inventory, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(bar_visibility(&mut app), Visibility::Hidden, "{mode:?}");
        }

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.world_mut().remove_resource::<Session>();
        app.update();
        assert_eq!(bar_visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn only_a_higher_level_after_the_baseline_starts_the_flash() {
        let mut app = hud(Some(vitals(7, 120, 350)));
        assert_eq!(flash_visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, vitals(7, 121, 350));
        assert_eq!(flash_visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, vitals(6, 20, 200));
        assert_eq!(flash_visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, vitals(8, 0, 500));
        assert_eq!(flash_visibility(&mut app), Visibility::Visible);
        assert_eq!(flash_label(&mut app), "Level 8");
        assert_eq!(flash_colour(&mut app), level_up_colour(1.0));
    }

    #[test]
    fn level_up_flash_fades_and_expires_on_real_time() {
        let mut app = hud(Some(vitals(7, 349, 350)));
        deliver(&mut app, vitals(8, 0, 500));
        assert_eq!(flash_colour(&mut app), level_up_colour(1.0));

        advance(&mut app, 2);
        assert_eq!(flash_visibility(&mut app), Visibility::Visible);
        assert_eq!(flash_colour(&mut app), level_up_colour(0.5));

        advance(&mut app, 1);
        assert_eq!(flash_visibility(&mut app), Visibility::Visible);
        advance(&mut app, 1);
        assert_eq!(flash_visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn losing_a_session_clears_the_flash_and_makes_resume_a_silent_baseline() {
        let mut app = hud(Some(vitals(7, 349, 350)));
        deliver(&mut app, vitals(8, 0, 500));
        assert_eq!(flash_visibility(&mut app), Visibility::Visible);

        app.world_mut().remove_resource::<Session>();
        app.update();
        assert_eq!(flash_visibility(&mut app), Visibility::Hidden);

        app.world_mut().insert_resource(session());
        deliver(&mut app, vitals(20, 0, 2_000));
        assert_eq!(flash_visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, vitals(21, 0, 2_500));
        assert_eq!(flash_visibility(&mut app), Visibility::Visible);
        assert_eq!(flash_label(&mut app), "Level 21");
    }

    #[test]
    fn replacing_a_session_also_makes_its_first_vitals_a_silent_baseline() {
        let mut app = hud(Some(vitals(7, 349, 350)));
        deliver(&mut app, vitals(8, 0, 500));
        assert_eq!(flash_visibility(&mut app), Visibility::Visible);

        let mut replacement = session();
        replacement.0.entity_id = 2;
        app.world_mut().insert_resource(replacement);
        deliver(&mut app, vitals(20, 0, 2_000));
        assert_eq!(flash_visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, vitals(21, 0, 2_500));
        assert_eq!(flash_visibility(&mut app), Visibility::Visible);
        assert_eq!(flash_label(&mut app), "Level 21");
    }
}
