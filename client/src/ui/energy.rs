//! The energy bar, directly under health, and the flash that answers a starved swing.
//!
//! **Nothing here decides anything.** The fill and the reading are the newest
//! [`PlayerVitals`] the server sent through [`SelfVitals`]: `energy / max_energy`, redrawn
//! when a snapshot replaces the vitals and at no other moment. There is no local cost
//! applied when the player swings and no local refill between snapshots — a client that
//! drew either would be predicting a number the server owns, and a bar that looked full
//! while the server said empty is exactly the swing this bar exists to explain. Silence
//! holds the last authoritative value on screen, the way the health bar holds its own.
//!
//! The one local clock is presentation. When the server refuses an attack because the
//! reserve cannot pay for it, `ui/status.rs` hands that refusal here as an
//! [`EnergyRefused`] instead of writing a chat line, and the track flashes briefly. The
//! flash says *when* the server's answer arrived; it never says anything about energy the
//! answer did not.
//!
//! **The track flashes, not the fill.** A refused swing is by construction one the reserve
//! could not pay for, so the fill is at its lowest exactly when the flash is needed: a
//! flash drawn in the fill would be a flash drawn in a sliver, or in nothing at all.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::Real;

use super::health::{
    BAR_LABEL_SIZE, DEATH_LAYER, ENERGY_BAR_BOTTOM, vital_bar_label, vital_bar_label_transform,
    vital_bar_root, vital_bar_track,
};
use super::{CELL_EDGE, PublishPlayerMessages};
use crate::net::{PlayerVitals, Session};
use crate::player::{ApplySnapshots, InputMode, SelfVitals};

/// The layer the other vital bars draw on. The flash lives inside the track, so it shares
/// it: nothing here is a full-screen overlay, and nothing can cover the death screen.
const ENERGY_LAYER: i32 = 12;

const _: () = assert!(
    ENERGY_LAYER < DEATH_LAYER,
    "the energy bar is HUD and must stay under the death overlay"
);

/// How long a refusal keeps the track lit. Long enough to register at the edge of vision
/// while the eyes are on a target, short enough that a second refused swing restarts a
/// visible flash rather than extending one that never went out.
const REFUSAL_FLASH_DURATION: Duration = Duration::from_millis(400);

/// The empty part of the track, shared visually with the other vital bars.
const BAR_TRACK: Color = Color::srgba(0.055, 0.065, 0.080, 0.94);

/// What energy is drawn in: a light, warm amber. Brighter and yellower than hunger's
/// deeper orange, and nowhere near health's red — the test beside the fill fraction holds
/// both distances rather than trusting a description of three colours.
pub(super) const BAR_FILL: Color = Color::srgb(0.98, 0.80, 0.30);

/// The track's edge at the peak of a refusal flash.
const REFUSAL_EDGE: Color = Color::srgb(1.0, 0.93, 0.70);

/// The track's empty interior at the peak of a refusal flash: the reserve that was not
/// there, lit dimly in the bar's own hue.
const REFUSAL_TRACK: Color = Color::srgba(0.46, 0.30, 0.06, 0.94);

/// One refused attack the energy bar answers.
///
/// Written by `ui/status.rs`, which drains the refusal inbox and knows which refusal has a
/// surface other than the chat log. A unit rather than a copy of the refusal, because the
/// bar has nothing to say about its fields: the answer is the flash.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EnergyRefused;

pub(super) struct EnergyUiPlugin;

impl Plugin for EnergyUiPlugin {
    fn build(&self, app: &mut App) {
        // The player plugin owns the first two in the game and `ui/mod.rs` registers the
        // message. Initialising them here keeps this module drivable on its own.
        app.init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<RefusalFlash>()
            .add_message::<EnergyRefused>()
            .add_systems(Startup, spawn_energy_bar)
            .add_systems(
                Update,
                (
                    refresh_energy_bar,
                    show_energy_bar,
                    // After the refusals are published, so a starved swing lights the bar
                    // on the frame its answer arrives rather than the one after it.
                    drive_refusal_flash.after(PublishPlayerMessages),
                )
                    .after(ApplySnapshots),
            );
    }
}

/// The bar and everything inside it. Hidden and shown as one node.
#[derive(Component)]
pub(super) struct EnergyRoot;

/// The bar's background and edge, which is where a refusal is drawn.
#[derive(Component)]
pub(super) struct EnergyTrack;

/// The filled part. Its width is the server's ratio.
#[derive(Component)]
struct EnergyFill;

/// The numeric reading inside the bar.
#[derive(Component)]
pub(super) struct EnergyLabel;

/// Presentation-only time left on the refusal flash. Zero is at rest.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RefusalFlash {
    remaining: Duration,
}

fn spawn_energy_bar(mut commands: Commands) {
    commands
        .spawn((
            EnergyRoot,
            vital_bar_root(ENERGY_BAR_BOTTOM),
            Visibility::Hidden,
            GlobalZIndex(ENERGY_LAYER),
        ))
        .with_children(|root| {
            root.spawn((
                EnergyTrack,
                vital_bar_track(),
                BackgroundColor(BAR_TRACK),
                BorderColor::all(CELL_EDGE),
            ))
            .with_children(|track| {
                track.spawn((
                    EnergyFill,
                    Node {
                        // Zero until a server snapshot supplies a ratio. A bar that started
                        // full would be this client asserting a reserve nobody has sent it.
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(BAR_FILL),
                ));
                track.spawn((
                    EnergyLabel,
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

/// Draws the newest authoritative energy.
///
/// Guarded on the change flag, for the reason the health bar is: everything drawn here is a
/// function of [`SelfVitals`] alone. It is also what makes *"holds the last value"*
/// structural — no clock appears in this system's parameters, so time passing between
/// snapshots cannot move the fill.
fn refresh_energy_bar(
    vitals: Res<SelfVitals>,
    mut fills: Query<&mut Node, With<EnergyFill>>,
    mut labels: Query<&mut Text, With<EnergyLabel>>,
) {
    if !vitals.is_changed() {
        return;
    }
    let Some(current) = vitals.get() else {
        // No snapshot yet, or a session that has ended. The bar keeps what it last drew
        // and `show_energy_bar` hides it.
        return;
    };

    let width = Val::Percent(fill_percent(current));
    for mut node in &mut fills {
        if node.width != width {
            node.width = width;
        }
    }

    let label = energy_label(current);
    for mut text in &mut labels {
        if text.0 != label {
            text.0.clone_from(&label);
        }
    }
}

/// Shows under exactly the health bar's conditions: playing, connected, and told vitals.
fn show_energy_bar(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut roots: Query<&mut Visibility, With<EnergyRoot>>,
) {
    let next = if bar_is_shown(&mode, session.is_some(), &vitals) {
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

/// Lights the track when the server refuses a swing for want of energy, then lets it fade.
///
/// Every queued refusal is drained each frame whether or not it lights anything. One that
/// arrives while the bar is hidden — behind the pack, the pause menu, before the first
/// vitals — lights nothing, and it does not wait: a flash carried over to whenever the bar
/// next appears would be answering a swing the player has long stopped thinking about.
/// Hiding the bar ends a flash already running for the same reason.
///
/// [`Time<Real>`] rather than the game clock, as hunger's reminder uses it: the length of a
/// flash is how long a player sees something, not a duration in the simulation.
fn drive_refusal_flash(
    time: Res<Time<Real>>,
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut refusals: MessageReader<EnergyRefused>,
    mut flash: ResMut<RefusalFlash>,
    mut tracks: Query<(&mut BackgroundColor, &mut BorderColor), With<EnergyTrack>>,
) {
    let arrived = refusals.read().count() > 0;
    let remaining = flash.remaining.saturating_sub(time.delta());
    let next = if !bar_is_shown(&mode, session.is_some(), &vitals) {
        Duration::ZERO
    } else if arrived {
        REFUSAL_FLASH_DURATION
    } else {
        remaining
    };
    if flash.remaining != next {
        flash.remaining = next;
    }

    let strength = flash.remaining.as_secs_f32() / REFUSAL_FLASH_DURATION.as_secs_f32();
    let background = BackgroundColor(blend(BAR_TRACK, REFUSAL_TRACK, strength));
    let edge = BorderColor::all(blend(CELL_EDGE, REFUSAL_EDGE, strength));
    for (mut fill, mut border) in &mut tracks {
        if *fill != background {
            *fill = background;
        }
        if *border != edge {
            *border = edge;
        }
    }
}

/// The one visibility rule both systems ask, so a flash can never run on a hidden bar.
fn bar_is_shown(mode: &InputMode, connected: bool, vitals: &SelfVitals) -> bool {
    matches!(*mode, InputMode::Playing | InputMode::Chat) && connected && vitals.get().is_some()
}

/// How much of the bar the server's energy fills, as a percentage of its width.
///
/// The decoder refuses a zero `max_energy` and an `energy` above it, so this divides by
/// the server's own non-zero number. The clamp is defensive presentation, as it is for
/// health: an overflowing fill looks merely full.
fn fill_percent(vitals: PlayerVitals) -> f32 {
    (f32::from(vitals.energy) * 100.0 / f32::from(vitals.max_energy)).clamp(0.0, 100.0)
}

/// The reading inside the bar: the server's two numbers and nothing derived from them.
fn energy_label(vitals: PlayerVitals) -> String {
    format!("{} / {}", vitals.energy, vitals.max_energy)
}

/// `rest` moved towards `peak` by `strength`, clamped at both ends so the resting and the
/// peak colours are exactly themselves rather than a float's approximation of them.
fn blend(rest: Color, peak: Color, strength: f32) -> Color {
    if strength <= 0.0 {
        return rest;
    }
    if strength >= 1.0 {
        return peak;
    }
    let (from, to) = (rest.to_srgba(), peak.to_srgba());
    let mix = |a: f32, b: f32| a + (b - a) * strength;
    Color::srgba(
        mix(from.red, to.red),
        mix(from.green, to.green),
        mix(from.blue, to.blue),
        mix(from.alpha, to.alpha),
    )
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<RefusalFlash>(world);
}

#[cfg(test)]
mod tests {
    //! Headless: `MinimalPlugins` and this plugin, asserted against nodes and colours.

    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{LifeState, SessionParams};
    use crate::ui::health::{
        BAR_HEIGHT, EXPERIENCE_BAR_BOTTOM, HEALTH_BAR_BOTTOM, HUNGER_BAR_BOTTOM, VITAL_BAR_GAP,
    };

    const STEP: Duration = Duration::from_millis(50);

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
            voice_range_blocks: 0.0,
        })
    }

    fn vitals(energy: u16, max_energy: u16) -> PlayerVitals {
        PlayerVitals {
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
            energy,
            max_energy,
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
        app.add_plugins(EnergyUiPlugin);
        app.update();
        app
    }

    fn deliver(app: &mut App, next: PlayerVitals) {
        app.insert_resource(SelfVitals::from_server(next));
        app.update();
    }

    fn refuse(app: &mut App) {
        app.world_mut().write_message(EnergyRefused);
        app.update();
    }

    fn advance(app: &mut App, frames: u32) {
        for _ in 0..frames {
            app.update();
        }
    }

    fn fill_width(app: &mut App) -> Val {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<EnergyFill>>();
        query.single(world).expect("one energy fill").width
    }

    fn label(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<EnergyLabel>>();
        query.single(world).expect("one energy label").0.clone()
    }

    fn visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<EnergyRoot>>();
        *query.single(world).expect("one energy root")
    }

    /// The track's interior and edge, as drawn this frame.
    fn track(app: &mut App) -> (Color, BorderColor) {
        let world = app.world_mut();
        let mut query =
            world.query_filtered::<(&BackgroundColor, &BorderColor), With<EnergyTrack>>();
        let (background, border) = query.single(world).expect("one energy track");
        (background.0, *border)
    }

    fn at_rest() -> (Color, BorderColor) {
        (BAR_TRACK, BorderColor::all(CELL_EDGE))
    }

    fn at_peak() -> (Color, BorderColor) {
        (REFUSAL_TRACK, BorderColor::all(REFUSAL_EDGE))
    }

    /// Frames for the flash to run out entirely at [`STEP`].
    fn flash_frames() -> u32 {
        (REFUSAL_FLASH_DURATION.as_millis() / STEP.as_millis()) as u32
    }

    #[test]
    fn the_fill_and_reading_are_exactly_the_servers_energy_over_its_maximum() {
        assert_eq!(fill_percent(vitals(100, 100)), 100.0);
        assert_eq!(fill_percent(vitals(0, 100)), 0.0);
        assert_eq!(fill_percent(vitals(3, 12)), 25.0);
        assert_eq!(fill_percent(vitals(u16::MAX, u16::MAX)), 100.0);
        assert_eq!(energy_label(vitals(u16::MAX, u16::MAX)), "65535 / 65535");

        let mut app = hud(Some(vitals(35, 100)));
        assert_eq!(fill_width(&mut app), Val::Percent(35.0));
        assert_eq!(label(&mut app), "35 / 100");

        deliver(&mut app, vitals(0, 80));
        assert_eq!(fill_width(&mut app), Val::Percent(0.0));
        assert_eq!(label(&mut app), "0 / 80");
    }

    /// No snapshot, no movement: not a local refill while the reserve is low, and not a
    /// local drain after a swing. The server's next number is the only thing that moves it.
    #[test]
    fn silence_holds_the_last_authoritative_value_with_no_regeneration() {
        let mut app = hud(Some(vitals(10, 100)));
        advance(&mut app, 200);
        assert_eq!(fill_width(&mut app), Val::Percent(10.0));
        assert_eq!(label(&mut app), "10 / 100");

        // A refusal is not a cost either: the flash does not touch the fill or the reading.
        refuse(&mut app);
        assert_eq!(fill_width(&mut app), Val::Percent(10.0));
        assert_eq!(label(&mut app), "10 / 100");
    }

    #[test]
    fn the_fill_is_a_warm_amber_distinct_from_health_and_hunger() {
        let distance = |a: Color, b: Color| {
            let (a, b) = (a.to_srgba(), b.to_srgba());
            (a.red - b.red)
                .abs()
                .max((a.green - b.green).abs())
                .max((a.blue - b.blue).abs())
        };
        for (name, other) in [
            ("health", crate::ui::health::BAR_FILL),
            ("hunger", crate::ui::hunger::BAR_FILL),
        ] {
            assert!(
                distance(BAR_FILL, other) >= 0.2,
                "the energy fill is too close to {name}'s to tell apart at a glance"
            );
        }
        let fill = BAR_FILL.to_srgba();
        assert!(
            fill.red > fill.green && fill.green > fill.blue,
            "amber: red over green over blue, got {fill:?}"
        );
    }

    /// The four always-on bars stack experience, hunger, energy, health from the hotbar up,
    /// one bar and the documented gap apart, and each is spawned where its constant says.
    #[test]
    fn energy_sits_directly_under_health_and_the_four_bars_never_overlap() {
        let stack = [
            ("experience", EXPERIENCE_BAR_BOTTOM),
            ("hunger", HUNGER_BAR_BOTTOM),
            ("energy", ENERGY_BAR_BOTTOM),
            ("health", HEALTH_BAR_BOTTOM),
        ];
        for pair in stack.windows(2) {
            let [(lower, lower_bottom), (upper, upper_bottom)] = pair else {
                unreachable!("windows(2) yields pairs");
            };
            assert_eq!(
                upper_bottom - lower_bottom,
                BAR_HEIGHT + VITAL_BAR_GAP,
                "{upper} must sit exactly one bar and the gap above {lower}"
            );
            assert!(
                lower_bottom + BAR_HEIGHT < *upper_bottom,
                "{lower} overlaps {upper}"
            );
        }

        let mut app = hud(Some(vitals(100, 100)));
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<EnergyRoot>>();
        let root = query.single(world).expect("one energy root");
        assert_eq!(root.bottom, Val::Px(ENERGY_BAR_BOTTOM));
        // The same column as every other vital bar: the shared root and track, by value.
        assert_eq!(*root, vital_bar_root(ENERGY_BAR_BOTTOM));
        let mut tracks = world.query_filtered::<&Node, With<EnergyTrack>>();
        assert_eq!(
            *tracks.single(world).expect("one energy track"),
            vital_bar_track()
        );
    }

    #[test]
    fn visibility_matches_the_other_vital_bars() {
        let mut app = hud(None);
        assert_eq!(visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, vitals(80, 100));
        assert_eq!(visibility(&mut app), Visibility::Visible);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Visible);

        for mode in [InputMode::Inventory, InputMode::Menu, InputMode::Map] {
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
    fn a_refusal_lights_the_track_on_its_frame_and_fades_back_to_rest() {
        let mut app = hud(Some(vitals(4, 100)));
        advance(&mut app, 3);
        assert_eq!(track(&mut app), at_rest());

        refuse(&mut app);
        assert_eq!(track(&mut app), at_peak());

        // Part-way through, it is neither: it is fading.
        advance(&mut app, flash_frames() / 2);
        let middle = track(&mut app);
        assert_ne!(middle, at_rest());
        assert_ne!(middle, at_peak());

        advance(&mut app, flash_frames());
        assert_eq!(track(&mut app), at_rest());
    }

    #[test]
    fn a_second_refusal_restarts_the_flash_at_full_strength() {
        let mut app = hud(Some(vitals(4, 100)));
        refuse(&mut app);
        advance(&mut app, flash_frames() - 2);
        assert_ne!(track(&mut app), at_peak());

        refuse(&mut app);
        assert_eq!(track(&mut app), at_peak());
        advance(&mut app, flash_frames() - 2);
        assert_ne!(
            track(&mut app),
            at_rest(),
            "the restarted flash runs its own full length"
        );
    }

    /// A refusal behind the pack lights nothing and is not saved for later; hiding the bar
    /// ends a flash already running.
    #[test]
    fn a_hidden_bar_neither_flashes_nor_carries_a_refusal_over() {
        let mut app = hud(Some(vitals(4, 100)));
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();
        refuse(&mut app);
        assert_eq!(track(&mut app), at_rest());

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Visible);
        assert_eq!(track(&mut app), at_rest());

        refuse(&mut app);
        assert_eq!(track(&mut app), at_peak());
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
        app.update();
        assert_eq!(track(&mut app), at_rest());
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert_eq!(track(&mut app), at_rest());
    }

    #[test]
    fn the_blend_is_exact_at_both_ends_and_between_them_in_between() {
        assert_eq!(blend(BAR_TRACK, REFUSAL_TRACK, 0.0), BAR_TRACK);
        assert_eq!(blend(BAR_TRACK, REFUSAL_TRACK, -1.0), BAR_TRACK);
        assert_eq!(blend(BAR_TRACK, REFUSAL_TRACK, 1.0), REFUSAL_TRACK);
        assert_eq!(blend(BAR_TRACK, REFUSAL_TRACK, 2.0), REFUSAL_TRACK);
        let half = blend(Color::srgb(0.0, 0.0, 0.0), Color::srgb(1.0, 0.5, 0.25), 0.5).to_srgba();
        assert_eq!((half.red, half.green, half.blue), (0.5, 0.25, 0.125));
    }

    #[test]
    fn a_world_change_puts_the_flash_out() {
        let mut app = hud(Some(vitals(4, 100)));
        refuse(&mut app);
        assert_ne!(
            *app.world().resource::<RefusalFlash>(),
            RefusalFlash::default()
        );
        reset_world(app.world_mut());
        assert_eq!(
            *app.world().resource::<RefusalFlash>(),
            RefusalFlash::default()
        );
    }
}
