//! The health bar, the server's respawn protection, and the death overlay.
//!
//! Permanent game UI, which is why it is here rather than in `ui/status.rs`: that module
//! exists to debug the transport, the streamed world and the player counters, and it is
//! the first thing a release build would stop drawing. Health is not a counter, it is the
//! thing the player is playing.
//!
//! **Nothing here decides anything.** Every number on screen is the newest `PlayerVitals`
//! the server sent, replaced wholesale by `player/mod.rs`:
//!
//! - The bar's fill is `health / max_health` and never a fraction of a constant this crate
//!   holds. Both numbers come off the wire together, and `net/codec.rs` has already
//!   refused a zero `max_health` and a `health` above it — so the ratio is well defined
//!   for every value that can reach this module.
//! - The countdown is `respawn_ticks` converted through `ServerWelcome.tick_rate` **for
//!   display**, and it is not a [`Timer`]. It moves when the server moves it and at no
//!   other moment: the string is rebuilt only when the vitals change, so silence holds the
//!   last authoritative number on screen rather than running it down. Same rule as the
//!   interpolation holding an entity's last position instead of extrapolating one.
//! - Respawn protection is drawn, never counted. The server owns that timer; `invulnerable`
//!   is its answer, and this module colours a border with it.
//!
//! The overlay is a `bevy_ui` node like every other panel here, drawn through the one
//! camera `player/camera.rs` owns. No second camera, no asset, no font file.

use bevy::prelude::*;

use super::{CELL_EDGE, CELL_SIZE};
use crate::net::{LifeState, PlayerVitals, Session};
use crate::player::{ApplySnapshots, InputMode, SelfVitals};

/// Width of the bar, in logical pixels.
pub(super) const BAR_WIDTH: f32 = 260.0;

/// Height of the bar, in logical pixels.
pub(super) const BAR_HEIGHT: f32 = 18.0;

/// Thickness of the bar's edge. Thinner than a cell border: this is one long node rather
/// than a grid, and the same weight would read as a frame around it.
pub(super) const BAR_BORDER: f32 = 2.0;

/// Distance from the bottom of the window to the experience bar, in logical pixels. It
/// clears the hotbar, which is [`CELL_SIZE`] tall and sits 18 px up.
pub(super) const EXPERIENCE_BAR_BOTTOM: f32 = 18.0 + CELL_SIZE + 14.0;

/// Vertical space between the three vital bars.
pub(super) const VITAL_BAR_GAP: f32 = 8.0;

/// Distance from the bottom of the window to the hunger bar. Experience takes the lower
/// position nearest the hotbar; hunger moves up by one bar and the documented gap.
pub(super) const HUNGER_BAR_BOTTOM: f32 = EXPERIENCE_BAR_BOTTOM + BAR_HEIGHT + VITAL_BAR_GAP;

/// Distance from the bottom of the window to this health bar. Health sits one bar and
/// the documented gap above hunger.
pub(super) const HEALTH_BAR_BOTTOM: f32 = HUNGER_BAR_BOTTOM + BAR_HEIGHT + VITAL_BAR_GAP;

/// The empty part of the bar. The same near-black the empty inventory cells use, so the
/// HUD reads as one surface.
const BAR_TRACK: Color = Color::srgba(0.055, 0.065, 0.080, 0.94);

/// What health is drawn in.
const BAR_FILL: Color = Color::srgb(0.72, 0.16, 0.16);

/// The bar's edge while the server is refusing damage. Ice against the blood, and the one
/// place this colour appears — a player should never have to compare two shades to know
/// whether they are protected.
const PROTECTED_EDGE: Color = Color::srgb(0.55, 0.85, 1.0);

/// Behind the death overlay. Dark and red rather than opaque: the world stays visible
/// through it, because a player who cannot see where they died learns nothing from it.
const DEATH_VEIL: Color = Color::srgba(0.10, 0.008, 0.012, 0.62);

/// The death overlay's layer. Above the crosshair (10) and the hotbar (12), below the
/// inventory (30) and — deliberately — below the pause menu (40): quitting and
/// disconnecting must never be buried under a death screen.
const DEATH_LAYER: i32 = 20;

/// What the countdown says before the server has named a number.
const NO_RESPAWN_YET: &str = "RESPAWNING";

pub(super) struct HealthUiPlugin;

impl Plugin for HealthUiPlugin {
    fn build(&self, app: &mut App) {
        // The player plugin owns both in the game. Initialising them here keeps this
        // module drivable on its own, which is what its tests do.
        app.init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .add_systems(Startup, (spawn_health_bar, spawn_death_overlay))
            .add_systems(
                Update,
                (
                    refresh_health_bar,
                    show_health_bar,
                    refresh_death_overlay,
                    show_death_overlay,
                )
                    // After the snapshot that carried the vitals has been applied, so a
                    // death and a respawn both reach the screen on the frame the server's
                    // answer arrives rather than the one after it. Ordering against an
                    // empty set is a no-op, which keeps this module testable with no
                    // player plugin built at all.
                    .after(ApplySnapshots),
            );
    }
}

/// The bar and everything inside it. Hidden and shown as one node.
#[derive(Component)]
struct HealthRoot;

/// The bar's background and edge. The edge is where respawn protection is drawn.
#[derive(Component)]
struct HealthTrack;

/// The filled part. Its width **is** the server's ratio.
#[derive(Component)]
struct HealthFill;

/// The numeric reading beside the bar, so a screenshot says what the bar means.
#[derive(Component)]
struct HealthLabel;

/// The death overlay's root.
#[derive(Component)]
struct DeathRoot;

/// The line that counts the server's remaining `respawn_ticks` down.
#[derive(Component)]
struct RespawnText;

fn spawn_health_bar(mut commands: Commands) {
    commands
        .spawn((
            HealthRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(HEALTH_BAR_BOTTOM),
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
                HealthTrack,
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
                HealthFill,
                Node {
                    // Zero until the server says otherwise. A bar that started full would
                    // be this client asserting a health nobody has sent it.
                    width: Val::Percent(0.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(BAR_FILL),
            ));

            root.spawn((
                HealthLabel,
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

fn spawn_death_overlay(mut commands: Commands) {
    commands
        .spawn((
            DeathRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(14.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(DEATH_VEIL),
            Visibility::Hidden,
            GlobalZIndex(DEATH_LAYER),
        ))
        .with_children(|overlay| {
            overlay.spawn((
                Text::new("YOU DIED"),
                TextFont {
                    font_size: FontSize::Px(52.0),
                    ..default()
                },
                TextColor(Color::srgb(0.86, 0.24, 0.22)),
                TextShadow::default(),
            ));
            overlay.spawn((
                RespawnText,
                Text::new(NO_RESPAWN_YET.to_owned()),
                TextFont {
                    font_size: FontSize::Px(22.0),
                    ..default()
                },
                TextColor(Color::WHITE),
                TextShadow::default(),
            ));
        });
}

/// Draws the newest authoritative health.
///
/// Guarded on the resource's change flag rather than recomputed every frame: everything
/// below is a function of [`SelfVitals`] alone, so a frame in which it did not move has
/// nothing new to say — and the label would otherwise reallocate its string sixty times a
/// second.
fn refresh_health_bar(
    vitals: Res<SelfVitals>,
    mut fills: Query<&mut Node, With<HealthFill>>,
    mut tracks: Query<&mut BorderColor, With<HealthTrack>>,
    mut labels: Query<&mut Text, With<HealthLabel>>,
) {
    if !vitals.is_changed() {
        return;
    }
    let Some(current) = vitals.get() else {
        // No snapshot yet, or a session that has ended. The bar keeps whatever it last
        // drew and `show_health_bar` hides it, exactly as the hotbar keeps its cells.
        return;
    };

    let width = Val::Percent(fill_percent(current));
    for mut node in &mut fills {
        if node.width != width {
            node.width = width;
        }
    }

    // The server's flag, drawn. There is no local immunity timer here and nowhere for one
    // to live: `invulnerable` changes when a snapshot changes it.
    let edge = BorderColor::all(if current.invulnerable {
        PROTECTED_EDGE
    } else {
        CELL_EDGE
    });
    for mut border in &mut tracks {
        if *border != edge {
            *border = edge;
        }
    }

    let label = format!("{} / {}", current.health, current.max_health);
    for mut text in &mut labels {
        if text.0 != label {
            text.0.clone_from(&label);
        }
    }
}

/// Shows the bar for a live playing session that has been told a health.
///
/// The same condition the hotbar and the crosshair use, plus the vitals themselves: a bar
/// drawn before the first snapshot would be a number this client made up, and a session
/// that has ended has no health to report.
fn show_health_bar(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut roots: Query<&mut Visibility, With<HealthRoot>>,
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

/// Writes the remaining respawn time the server is counting down.
///
/// Stale is defined by the two things the line is made of, and by nothing else — no clock
/// appears in this system's parameters, which is what makes *"the countdown holds when
/// snapshots stop"* structural rather than a promise. Local time can pass for as long as
/// it likes; the string is not rebuilt, because the number it renders has not moved.
fn refresh_death_overlay(
    vitals: Res<SelfVitals>,
    session: Option<Res<Session>>,
    mut nodes: Query<&mut Text, With<RespawnText>>,
) {
    let stale = vitals.is_changed() || session.as_ref().is_some_and(|session| session.is_changed());
    if !stale {
        return;
    }

    let (Some(current), Some(session)) = (vitals.get(), session.as_deref()) else {
        return;
    };
    if current.life_state != LifeState::Dead {
        // Left exactly as it was. The overlay is hidden, and rewriting a line nobody can
        // read would only spend the allocation this guard exists to avoid.
        return;
    }

    let line = respawn_line(current.respawn_ticks, session.0.tick_rate);
    for mut text in &mut nodes {
        if text.0 != line {
            text.0.clone_from(&line);
        }
    }
}

/// Shows the overlay exactly while the server says this player is dead.
///
/// Not while the client suspects it, and not for a frame longer: this runs after the
/// snapshot application set, so the newer snapshot that says `Alive` takes the overlay
/// away on the same frame it restores the health above.
fn show_death_overlay(
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut roots: Query<&mut Visibility, With<DeathRoot>>,
) {
    let next = if session.is_some() && vitals.dead() {
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

/// How much of the bar the server's health fills, as a percentage of its width.
///
/// `max_health` is non-zero by decoder invariant — `net/codec.rs` refuses a zero before a
/// `PlayerVitals` exists at all — so this divides by the server's own number and never by
/// a constant here, and there is no reachable state in which it divides by zero. That is
/// the same guarantee `tick_rate` carries.
///
/// The clamp is not load-bearing: `health <= max_health` is the other half of that
/// invariant. It is written down because a bar is one of the few places where being wrong
/// is invisible — a fill of 140% draws exactly like a fill of 100%.
fn fill_percent(vitals: PlayerVitals) -> f32 {
    (f32::from(vitals.health) * 100.0 / f32::from(vitals.max_health)).clamp(0.0, 100.0)
}

/// What the overlay says about the respawn the server is counting down to.
///
/// A conversion for display and nothing else. The count belongs to the server; this turns
/// its ticks into the seconds a player reads and never subtracts one of its own.
fn respawn_line(respawn_ticks: u32, tick_rate: u8) -> String {
    if respawn_ticks == 0 {
        // Either the server has not put a count on this death yet, or the count has run
        // out and the respawn is on its way. Showing "0.0s" would be the client naming the
        // frame the player comes back, which is the server's to name.
        return NO_RESPAWN_YET.to_owned();
    }

    // `tick_rate >= 1` is a `SessionParams` invariant, so this is the server's announced
    // rate and never a zero.
    let seconds = f64::from(respawn_ticks) / f64::from(tick_rate);
    format!("RESPAWNING IN {seconds:.1}s")
}

#[cfg(test)]
mod tests {
    //! No window, no display and no GPU — `MinimalPlugins` and this plugin are the whole
    //! app. Every assertion below is against a node, a colour or a string, because "the
    //! bar looks right" is a screenshot and "the fill is exactly the server's ratio" is a
    //! test.

    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::SessionParams;

    const TICK_RATE: u8 = 20;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: TICK_RATE,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 36,
            hotbar_slots: 9,
            equipment_slots: 3,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn vitals(health: u16, max_health: u16) -> PlayerVitals {
        PlayerVitals {
            health,
            max_health,
            hunger: 100,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state: LifeState::Alive,
            respawn_ticks: 0,
            invulnerable: false,
        }
    }

    fn dead(respawn_ticks: u32) -> PlayerVitals {
        PlayerVitals {
            health: 0,
            max_health: 100,
            hunger: 50,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state: LifeState::Dead,
            respawn_ticks,
            invulnerable: false,
        }
    }

    /// This module on a headless app, with a session and the server's first answer.
    fn hud(first: PlayerVitals) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(SelfVitals::from_server(first))
            .add_plugins(HealthUiPlugin);
        app.update();
        app
    }

    /// Replaces the resource exactly as an accepted snapshot does.
    fn deliver(app: &mut App, next: PlayerVitals) {
        app.insert_resource(SelfVitals::from_server(next));
        app.update();
    }

    fn fill_width(app: &mut App) -> Val {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<HealthFill>>();
        query.single(world).expect("one health fill").width
    }

    fn track_edge(app: &mut App) -> BorderColor {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&BorderColor, With<HealthTrack>>();
        *query.single(world).expect("one health track")
    }

    fn label(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<HealthLabel>>();
        query.single(world).expect("one health label").0.clone()
    }

    fn bar_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<HealthRoot>>();
        *query.single(world).expect("one health root")
    }

    fn death_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<DeathRoot>>();
        *query.single(world).expect("one death overlay")
    }

    fn respawn_text(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<RespawnText>>();
        query.single(world).expect("one respawn line").0.clone()
    }

    // ---------------------------------------------------------------------------
    // The ratio
    // ---------------------------------------------------------------------------

    #[test]
    fn the_fill_is_exactly_the_servers_health_over_its_maximum() {
        assert_eq!(fill_percent(vitals(100, 100)), 100.0);
        assert_eq!(fill_percent(vitals(62, 100)), 62.0);
        assert_eq!(fill_percent(vitals(0, 100)), 0.0);
        // A maximum that is not the client's idea of one. Nothing here divides by a
        // constant, so an unfamiliar denominator is simply the denominator.
        assert_eq!(fill_percent(vitals(3, 12)), 25.0);
        assert_eq!(fill_percent(vitals(1, 1)), 100.0);
        assert_eq!(fill_percent(vitals(u16::MAX, u16::MAX)), 100.0);

        // Every schema-valid ratio stays inside the bar and remains a number.
        let one_third = fill_percent(vitals(1, 3));
        assert!(
            (one_third - 100.0 / 3.0).abs() < 1e-3,
            "a ratio that is not exact in binary is still the server's ratio: {one_third}"
        );
    }

    #[test]
    fn full_partial_and_zero_health_all_reach_the_node() {
        let mut app = hud(vitals(100, 100));
        assert_eq!(fill_width(&mut app), Val::Percent(100.0));
        assert_eq!(label(&mut app), "100 / 100");

        deliver(&mut app, vitals(62, 100));
        assert_eq!(fill_width(&mut app), Val::Percent(62.0));
        assert_eq!(label(&mut app), "62 / 100");

        deliver(&mut app, dead(0));
        assert_eq!(fill_width(&mut app), Val::Percent(0.0));
        assert_eq!(label(&mut app), "0 / 100");
    }

    #[test]
    fn the_bar_is_hidden_until_the_server_has_sent_a_health() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .add_plugins(HealthUiPlugin);
        app.update();

        assert_eq!(bar_visibility(&mut app), Visibility::Hidden);
        assert_eq!(
            fill_width(&mut app),
            Val::Percent(0.0),
            "an empty bar, never a full one, before the server has said anything"
        );

        deliver(&mut app, vitals(40, 80));
        assert_eq!(bar_visibility(&mut app), Visibility::Visible);
        assert_eq!(fill_width(&mut app), Val::Percent(50.0));
    }

    // ---------------------------------------------------------------------------
    // Respawn protection
    // ---------------------------------------------------------------------------

    #[test]
    fn the_servers_protection_flag_is_drawn_and_never_counted() {
        let mut app = hud(vitals(100, 100));
        assert_eq!(track_edge(&mut app), BorderColor::all(CELL_EDGE));

        deliver(
            &mut app,
            PlayerVitals {
                invulnerable: true,
                ..vitals(100, 100)
            },
        );
        assert_eq!(track_edge(&mut app), BorderColor::all(PROTECTED_EDGE));

        // Local time passing changes nothing: there is no timer here to expire. Only the
        // server withdrawing the flag does.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(5)));
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(track_edge(&mut app), BorderColor::all(PROTECTED_EDGE));

        deliver(&mut app, vitals(100, 100));
        assert_eq!(track_edge(&mut app), BorderColor::all(CELL_EDGE));
    }

    // ---------------------------------------------------------------------------
    // Death and respawn
    // ---------------------------------------------------------------------------

    #[test]
    fn the_countdown_is_the_servers_ticks_at_the_servers_rate() {
        assert_eq!(respawn_line(60, 20), "RESPAWNING IN 3.0s");
        assert_eq!(respawn_line(1, 20), "RESPAWNING IN 0.1s");
        assert_eq!(respawn_line(7, 3), "RESPAWNING IN 2.3s");
        // The one rate the contract's floor allows, and the one it ceilings at.
        assert_eq!(respawn_line(5, 1), "RESPAWNING IN 5.0s");
        assert_eq!(respawn_line(u32::MAX, u8::MAX), "RESPAWNING IN 16843009.0s");
        // No count is not a zero count: naming the frame the player returns is the
        // server's to name.
        assert_eq!(respawn_line(0, 20), NO_RESPAWN_YET);
    }

    #[test]
    fn death_shows_the_overlay_and_the_authoritative_countdown() {
        let mut app = hud(vitals(100, 100));
        assert_eq!(death_visibility(&mut app), Visibility::Hidden);

        deliver(&mut app, dead(60));
        assert_eq!(death_visibility(&mut app), Visibility::Visible);
        assert_eq!(respawn_text(&mut app), "RESPAWNING IN 3.0s");
        assert_eq!(fill_width(&mut app), Val::Percent(0.0));

        deliver(&mut app, dead(20));
        assert_eq!(respawn_text(&mut app), "RESPAWNING IN 1.0s");
    }

    #[test]
    fn the_countdown_holds_when_snapshots_stop_and_never_runs_down_locally() {
        let mut app = hud(dead(60));
        assert_eq!(respawn_text(&mut app), "RESPAWNING IN 3.0s");

        // Ten seconds of local time against a three-second count. A `Timer` would have
        // fired six times over; this holds, because the only thing that moves the number
        // is another snapshot.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
        for _ in 0..10 {
            app.update();
            assert_eq!(respawn_text(&mut app), "RESPAWNING IN 3.0s");
            assert_eq!(death_visibility(&mut app), Visibility::Visible);
        }

        // And the value itself is where the server left it. No system in this module takes
        // it mutably, so nothing on screen can have moved a health, a respawn count or an
        // invulnerability flag — this asserts the consequence rather than the signature.
        assert_eq!(
            *app.world().resource::<SelfVitals>(),
            SelfVitals::from_server(dead(60))
        );
    }

    #[test]
    fn returning_to_alive_clears_the_overlay_on_the_frame_the_snapshot_lands() {
        let mut app = hud(dead(20));
        assert_eq!(death_visibility(&mut app), Visibility::Visible);

        deliver(&mut app, vitals(100, 100));
        assert_eq!(
            death_visibility(&mut app),
            Visibility::Hidden,
            "one update, not two: the overlay goes when the server says the player is back"
        );
        assert_eq!(fill_width(&mut app), Val::Percent(100.0));
        assert_eq!(label(&mut app), "100 / 100");
    }

    #[test]
    fn a_session_that_ends_hides_the_bar_and_the_overlay() {
        let mut app = hud(dead(40));
        assert_eq!(death_visibility(&mut app), Visibility::Visible);

        // What `drain_session_events` does, plus what `forget_vitals_without_a_session`
        // does behind it.
        app.world_mut().remove_resource::<Session>();
        app.insert_resource(SelfVitals::default());
        app.update();

        assert_eq!(bar_visibility(&mut app), Visibility::Hidden);
        assert_eq!(death_visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn the_pause_menu_and_the_inventory_hide_the_bar_but_not_the_death_overlay() {
        let mut app = hud(dead(40));

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        assert_eq!(bar_visibility(&mut app), Visibility::Visible);

        for mode in [InputMode::Inventory, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(
                bar_visibility(&mut app),
                Visibility::Hidden,
                "mode {mode:?}"
            );
            assert_eq!(
                death_visibility(&mut app),
                Visibility::Visible,
                "the death overlay answers to the server, not to a UI mode ({mode:?})"
            );
        }
    }
}
