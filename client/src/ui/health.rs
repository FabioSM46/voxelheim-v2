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
//! - The vignette is opacity mapped from the same health ratio, with a transparent centre.
//!   It changes only when a newer accepted snapshot replaces [`SelfVitals`].
//! - The eyelids become eligible from the server's countdown, after the existing camera
//!   fall has completed. Local time draws their closure but never opens them: only the
//!   next authoritative Alive state removes the black frame.
//!
//! The overlay is a `bevy_ui` node like every other panel here, drawn through the one
//! camera `player/camera.rs` owns. No second camera, no asset, no font file.

use bevy::prelude::*;
use bevy::ui::{ColorStop, FocusPolicy};
use std::time::Duration;

use super::menu::MENU_LAYER;
use super::{CELL_EDGE, CELL_SIZE};
use crate::net::{DrainNetwork, LifeState, MobHit, MobHitInbox, PlayerVitals, Session};
use crate::player::{
    AimCamera, ApplySnapshots, DeathFall, InputMode, LocalPlayer, SelfVitals, WorldCamera,
};

/// Width of the bar, in logical pixels.
pub(super) const BAR_WIDTH: f32 = 260.0;

/// Height of the bar, in logical pixels.
pub(super) const BAR_HEIGHT: f32 = 18.0;

/// Thickness of the bar's edge. Thinner than a cell border: this is one long node rather
/// than a grid, and the same weight would read as a frame around it.
pub(super) const BAR_BORDER: f32 = 2.0;

/// Horizontal space between a vital track and its numeric reading.
pub(super) const BAR_COLUMN_GAP: f32 = 10.0;

/// Width reserved for every vital label. The longest wire-valid experience reading
/// (`u16` level and two `u32` progression values) fits without wrapping, and shorter
/// health or hunger readings occupy the same column instead of moving their track.
pub(super) const BAR_LABEL_WIDTH: f32 = 360.0;

/// Shared vital label size, in logical pixels.
pub(super) const BAR_LABEL_SIZE: f32 = 17.0;

const BAR_CORNER_RADIUS: f32 = 3.0;

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

/// A brief peripheral warning: visible enough to register, faint enough not to hide play.
const HIT_PULSE_DURATION: Duration = Duration::from_millis(300);
const HIT_PULSE_ALPHA: f32 = 0.14;
const HIT_PULSE_LAYER: i32 = 19;

/// The darkest a living low-health edge becomes. The centre stays transparent at every
/// value; this is only the opacity at the outside of the screen.
const VIGNETTE_MAX_ALPHA: f32 = 0.72;
const VIGNETTE_CLEAR_PERCENT: f32 = 52.0;
const VIGNETTE_LAYER: i32 = 18;

/// The final authoritative countdown window is one eyelid closure plus one full second
/// of black. Local time draws the first part; only an Alive snapshot ends the second.
const EYELID_CLOSE_DURATION: Duration = Duration::from_millis(300);
const FINAL_BLACK_DURATION: Duration = Duration::from_secs(1);
const DEATH_TRANSITION_LAYER: i32 = 35;

const _: () = assert!(
    VIGNETTE_LAYER < HIT_PULSE_LAYER
        && HIT_PULSE_LAYER < DEATH_LAYER
        && DEATH_LAYER < DEATH_TRANSITION_LAYER
        && DEATH_TRANSITION_LAYER < MENU_LAYER
);

/// What the countdown says before the server has named a number.
const NO_RESPAWN_YET: &str = "RESPAWNING";

pub(super) struct HealthUiPlugin;

impl Plugin for HealthUiPlugin {
    fn build(&self, app: &mut App) {
        // The player plugin owns both in the game. Initialising them here keeps this
        // module drivable on its own, which is what its tests do.
        app.init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<MobHitInbox>()
            .init_resource::<HitPulse>()
            .init_resource::<DeathTransition>()
            .add_systems(
                Startup,
                (
                    spawn_health_bar,
                    spawn_low_health_vignette,
                    spawn_death_overlay,
                    spawn_death_transition,
                    spawn_hit_pulse,
                ),
            )
            .add_systems(
                Update,
                (
                    refresh_health_bar,
                    show_health_bar,
                    refresh_low_health_vignette,
                    refresh_death_overlay,
                    show_death_overlay,
                    drive_death_transition,
                    drive_hit_pulse,
                )
                    // After the snapshot that carried the vitals has been applied, so a
                    // death and a respawn both reach the screen on the frame the server's
                    // answer arrives rather than the one after it. Ordering against an
                    // empty set is a no-op, which keeps this module testable with no
                    // player plugin built at all.
                    .after(ApplySnapshots)
                    .after(DrainNetwork)
                    .after(AimCamera),
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

/// A radial gradient whose transparent centre leaves the player's focus untouched.
#[derive(Component)]
struct LowHealthVignette;

/// The layer that eventually covers the death text, but never the pause menu.
#[derive(Component)]
struct DeathTransitionRoot;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum Eyelid {
    Upper,
    Lower,
}

#[derive(Component)]
struct HitPulseRoot;

#[derive(Resource, Default)]
struct HitPulse {
    remaining: Duration,
}

/// The common row contract for health, hunger and experience.
///
/// All three roots span the window and centre the same fixed-width track/label pair.
/// Keeping the label column fixed makes the track's horizontal geometry independent of
/// the text it contains, while the non-shrinking children preserve the shared dimensions.
pub(super) fn vital_bar_root(bottom: f32) -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(0.0),
        right: Val::Px(0.0),
        bottom: Val::Px(bottom),
        display: Display::Flex,
        column_gap: Val::Px(BAR_COLUMN_GAP),
        align_items: AlignItems::Center,
        justify_content: JustifyContent::Center,
        ..default()
    }
}

pub(super) fn vital_bar_track() -> Node {
    Node {
        width: Val::Px(BAR_WIDTH),
        height: Val::Px(BAR_HEIGHT),
        flex_shrink: 0.0,
        border: UiRect::all(Val::Px(BAR_BORDER)),
        border_radius: BorderRadius::all(Val::Px(BAR_CORNER_RADIUS)),
        ..default()
    }
}

pub(super) fn vital_bar_label() -> Node {
    Node {
        width: Val::Px(BAR_LABEL_WIDTH),
        min_width: Val::Px(BAR_LABEL_WIDTH),
        max_width: Val::Px(BAR_LABEL_WIDTH),
        flex_shrink: 0.0,
        ..default()
    }
}

/// Local presentation time after the authoritative countdown enters its final window.
/// `None` is not yet closing; a saturated duration is black until Alive arrives.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DeathTransition {
    elapsed: Option<Duration>,
}

fn spawn_health_bar(mut commands: Commands) {
    commands
        .spawn((
            HealthRoot,
            vital_bar_root(HEALTH_BAR_BOTTOM),
            Visibility::Hidden,
            GlobalZIndex(12),
        ))
        .with_children(|root| {
            root.spawn((
                HealthTrack,
                vital_bar_track(),
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
                vital_bar_label(),
                Text::new(String::new()),
                TextFont {
                    font_size: FontSize::Px(BAR_LABEL_SIZE),
                    ..default()
                },
                TextColor(Color::WHITE),
                TextLayout::no_wrap(),
                TextShadow::default(),
            ));
        });
}

fn spawn_low_health_vignette(mut commands: Commands) {
    commands.spawn((
        LowHealthVignette,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundGradient::from(vignette_gradient(0.0)),
        FocusPolicy::Pass,
        Visibility::Hidden,
        GlobalZIndex(VIGNETTE_LAYER),
    ));
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

fn spawn_death_transition(mut commands: Commands) {
    commands
        .spawn((
            DeathTransitionRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            FocusPolicy::Pass,
            Visibility::Hidden,
            GlobalZIndex(DEATH_TRANSITION_LAYER),
        ))
        .with_children(|root| {
            for eyelid in [Eyelid::Upper, Eyelid::Lower] {
                let mut node = Node {
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(0.0),
                    ..default()
                };
                match eyelid {
                    Eyelid::Upper => node.top = Val::Px(0.0),
                    Eyelid::Lower => node.bottom = Val::Px(0.0),
                }
                root.spawn((
                    eyelid,
                    node,
                    BackgroundColor(Color::BLACK),
                    FocusPolicy::Pass,
                ));
            }
        });
}

fn spawn_hit_pulse(mut commands: Commands) {
    commands.spawn((
        HitPulseRoot,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::srgba(1.0, 0.0, 0.0, 0.0)),
        Visibility::Hidden,
        GlobalZIndex(HIT_PULSE_LAYER),
    ));
}

/// Draws loss of peripheral visibility from the newest authoritative health ratio.
/// The radial gradient is transparent through the centre, and the whole node disappears
/// at full health and while dead rather than becoming a second death overlay.
fn refresh_low_health_vignette(
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut vignettes: Query<(&mut BackgroundGradient, &mut Visibility), With<LowHealthVignette>>,
) {
    let current = vitals.get();
    let alpha = current.map_or(0.0, vignette_alpha);
    let visible = session.is_some()
        && current.is_some_and(|vitals| vitals.life_state == LifeState::Alive)
        && alpha > 0.0;
    let next_visibility = if visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let next_gradient = vitals
        .is_changed()
        .then(|| BackgroundGradient::from(vignette_gradient(alpha)));

    for (mut gradient, mut visibility) in &mut vignettes {
        if let Some(next) = &next_gradient
            && *gradient != *next
        {
            *gradient = next.clone();
        }
        if *visibility != next_visibility {
            *visibility = next_visibility;
        }
    }
}

/// Closes the eyes only once both authoritative inputs make it eligible: the player is
/// still Dead near the end of the server's countdown, and the existing body/camera fall
/// has completed. Once begun, local time only draws the 300 ms closure. It can never open
/// the eyes or declare a respawn; black holds until the server sends Alive.
fn drive_death_transition(
    time: Res<Time>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    falls: Query<&DeathFall, With<LocalPlayer>>,
    mut transition: ResMut<DeathTransition>,
    mut roots: Query<&mut Visibility, With<DeathTransitionRoot>>,
    mut eyelids: Query<(&Eyelid, &mut Node)>,
) {
    let Some(current) = vitals.get() else {
        clear_death_transition(&mut transition, &mut roots, &mut eyelids);
        return;
    };
    let Some(session) = session.as_deref() else {
        clear_death_transition(&mut transition, &mut roots, &mut eyelids);
        return;
    };
    if current.life_state == LifeState::Alive {
        clear_death_transition(&mut transition, &mut roots, &mut eyelids);
        return;
    }

    if let Some(elapsed) = transition.elapsed {
        let next = (elapsed + time.delta()).min(EYELID_CLOSE_DURATION);
        if next != elapsed {
            transition.elapsed = Some(next);
        }
    } else {
        let fall_finished = falls.single().is_ok_and(|fall| fall.finished());
        if fall_finished && death_transition_due(current, session.0.tick_rate) {
            // The threshold belongs to this frame, so its preceding delta cannot be
            // charged to an animation that had not started yet.
            transition.elapsed = Some(Duration::ZERO);
        }
    }

    let progress = transition_progress(*transition);
    let visibility = if transition.elapsed.is_some() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut shown in &mut roots {
        if *shown != visibility {
            *shown = visibility;
        }
    }
    set_eyelid_progress(progress, &mut eyelids);
}

fn clear_death_transition(
    transition: &mut DeathTransition,
    roots: &mut Query<&mut Visibility, With<DeathTransitionRoot>>,
    eyelids: &mut Query<(&Eyelid, &mut Node)>,
) {
    if *transition != DeathTransition::default() {
        *transition = DeathTransition::default();
    }
    for mut shown in roots {
        if *shown != Visibility::Hidden {
            *shown = Visibility::Hidden;
        }
    }
    set_eyelid_progress(0.0, eyelids);
}

fn set_eyelid_progress(progress: f32, eyelids: &mut Query<(&Eyelid, &mut Node)>) {
    let height = Val::Percent(50.0 * progress.clamp(0.0, 1.0));
    for (_, mut node) in eyelids {
        if node.height != height {
            node.height = height;
        }
    }
}

/// Drains every hit once. Any attacker outside the active camera frustum restarts one
/// shared fade; inside attackers and vitals changes cannot touch it.
fn drive_hit_pulse(
    time: Res<Time>,
    mut inbox: ResMut<MobHitInbox>,
    // WorldCamera is spawned as a root and is never parented. Transform is intentional:
    // AimCamera mutates it earlier in this Update, before GlobalTransform propagation.
    cameras: Query<(&Camera, &Projection, &Transform), With<WorldCamera>>,
    mut pulse: ResMut<HitPulse>,
    mut roots: Query<(&mut BackgroundColor, &mut Visibility), With<HitPulseRoot>>,
) {
    pulse.remaining = pulse.remaining.saturating_sub(time.delta());

    let active_camera = cameras.iter().find(|(camera, _, _)| camera.is_active);
    let outside = active_camera.is_some_and(|(_, projection, transform)| {
        inbox
            .take()
            .into_iter()
            .any(|hit| !inside_frustum(hit, projection, transform))
    });
    if active_camera.is_none() {
        inbox.take();
    }
    if outside {
        pulse.remaining = HIT_PULSE_DURATION;
    }

    let alpha = HIT_PULSE_ALPHA * pulse.remaining.as_secs_f32() / HIT_PULSE_DURATION.as_secs_f32();
    let visibility = if !pulse.remaining.is_zero() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for (mut colour, mut shown) in &mut roots {
        *colour = BackgroundColor(Color::srgba(1.0, 0.0, 0.0, alpha));
        *shown = visibility;
    }
}

fn inside_frustum(hit: MobHit, projection: &Projection, camera: &Transform) -> bool {
    let world = Vec3::from_array(hit.attacker_pos);
    let view = camera.compute_affine().inverse().transform_point3(world);
    if view.z >= 0.0 {
        return false;
    }
    let ndc = projection.get_clip_from_view().project_point3(view);
    ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0 && (0.0..=1.0).contains(&ndc.z)
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

/// Edge opacity for a schema-valid authoritative health value.
///
/// Linear loss makes every decrease darker and every increase lighter on the snapshot
/// that carries it. One living point remains just below the configured bound; zero is the
/// only value that reaches it, and life state is deliberately not inferred from that.
fn vignette_alpha(vitals: PlayerVitals) -> f32 {
    let ratio = f32::from(vitals.health) / f32::from(vitals.max_health);
    (VIGNETTE_MAX_ALPHA * (1.0 - ratio)).clamp(0.0, VIGNETTE_MAX_ALPHA)
}

/// A transparent centre and one darkening edge, shaped to the current node rather than
/// to a fixed aspect ratio. Opacity changes; the unobscured centre never does.
fn vignette_gradient(alpha: f32) -> RadialGradient {
    let clear = Color::srgba(0.0, 0.0, 0.0, 0.0);
    RadialGradient::new(
        UiPosition::CENTER,
        RadialGradientShape::FarthestCorner,
        vec![
            ColorStop::percent(clear, 0.0),
            ColorStop::percent(clear, VIGNETTE_CLEAR_PERCENT),
            ColorStop::percent(
                Color::srgba(0.0, 0.0, 0.0, alpha.clamp(0.0, VIGNETTE_MAX_ALPHA)),
                100.0,
            ),
        ],
    )
}

/// Whether the server's latest countdown has entered the final closure-plus-black window.
/// Zero is eligible because it is either the exhausted count or a death whose first count
/// has not arrived; the completed fall still prevents an immediate cut to black.
fn death_transition_due(vitals: PlayerVitals, tick_rate: u8) -> bool {
    if vitals.life_state != LifeState::Dead {
        return false;
    }
    if vitals.respawn_ticks == 0 {
        return true;
    }
    let remaining = Duration::from_secs_f64(f64::from(vitals.respawn_ticks) / f64::from(tick_rate));
    remaining <= EYELID_CLOSE_DURATION + FINAL_BLACK_DURATION
}

fn transition_progress(transition: DeathTransition) -> f32 {
    transition.elapsed.map_or(0.0, |elapsed| {
        (elapsed.as_secs_f32() / EYELID_CLOSE_DURATION.as_secs_f32()).clamp(0.0, 1.0)
    })
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

    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::SessionParams;
    use crate::ui::experience::{
        ExperienceLabel, ExperienceRoot, ExperienceTrack, ExperienceUiPlugin,
    };
    use crate::ui::hunger::{HungerLabel, HungerRoot, HungerTrack, HungerUiPlugin};

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
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
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
            blocking: false,
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
            blocking: false,
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

    fn node<T: Component>(app: &mut App) -> Node {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<T>>();
        query.single(world).expect("one matching node").clone()
    }

    fn text_layout<T: Component>(app: &mut App) -> TextLayout {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&TextLayout, With<T>>();
        *query.single(world).expect("one matching text layout")
    }

    fn horizontal_root_contract(node: &Node) -> (Val, Val, Val, AlignItems, JustifyContent) {
        (
            node.left,
            node.right,
            node.column_gap,
            node.align_items,
            node.justify_content,
        )
    }

    fn track_edges(viewport_width: f32, root: &Node, track: &Node, label: &Node) -> (f32, f32) {
        assert_eq!(root.left, Val::Px(0.0));
        assert_eq!(root.right, Val::Px(0.0));
        assert_eq!(root.justify_content, JustifyContent::Center);
        let Val::Px(track_width) = track.width else {
            panic!("vital track width is not fixed");
        };
        let Val::Px(label_width) = label.width else {
            panic!("vital label width is not fixed");
        };
        let Val::Px(gap) = root.column_gap else {
            panic!("vital track/label gap is not fixed");
        };
        let left = (viewport_width - track_width - gap - label_width) / 2.0;
        (left, left + track_width)
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

    #[test]
    fn all_vital_tracks_share_one_horizontal_axis_for_short_and_long_labels() {
        let shortest = PlayerVitals {
            health: 0,
            max_health: 1,
            hunger: 0,
            max_hunger: 1,
            level: 0,
            experience: 0,
            experience_to_next: 1,
            life_state: LifeState::Alive,
            respawn_ticks: 0,
            invulnerable: false,
            blocking: false,
        };
        let longest = PlayerVitals {
            health: u16::MAX,
            max_health: u16::MAX,
            hunger: u16::MAX,
            max_hunger: u16::MAX,
            level: u16::MAX,
            experience: u32::MAX,
            experience_to_next: u32::MAX,
            life_state: LifeState::Alive,
            respawn_ticks: 0,
            invulnerable: false,
            blocking: false,
        };

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(SelfVitals::from_server(shortest))
            .add_plugins((HealthUiPlugin, HungerUiPlugin, ExperienceUiPlugin));
        app.update();

        let health_root = node::<HealthRoot>(&mut app);
        let hunger_root = node::<HungerRoot>(&mut app);
        let experience_root = node::<ExperienceRoot>(&mut app);
        assert_eq!(
            horizontal_root_contract(&health_root),
            horizontal_root_contract(&hunger_root)
        );
        assert_eq!(
            horizontal_root_contract(&health_root),
            horizontal_root_contract(&experience_root)
        );

        let health_track = node::<HealthTrack>(&mut app);
        let hunger_track = node::<HungerTrack>(&mut app);
        let experience_track = node::<ExperienceTrack>(&mut app);
        assert_eq!(health_track, hunger_track);
        assert_eq!(health_track, experience_track);

        let health_label = node::<HealthLabel>(&mut app);
        let hunger_label = node::<HungerLabel>(&mut app);
        let experience_label = node::<ExperienceLabel>(&mut app);
        assert_eq!(health_label, hunger_label);
        assert_eq!(health_label, experience_label);
        assert_eq!(health_label.width, Val::Px(BAR_LABEL_WIDTH));
        assert_eq!(health_label.min_width, Val::Px(BAR_LABEL_WIDTH));
        assert_eq!(health_label.max_width, Val::Px(BAR_LABEL_WIDTH));
        for layout in [
            text_layout::<HealthLabel>(&mut app),
            text_layout::<HungerLabel>(&mut app),
            text_layout::<ExperienceLabel>(&mut app),
        ] {
            assert_eq!(layout.linebreak, LineBreak::NoWrap);
        }

        for viewport_width in [800.0, 1280.0, 1920.0] {
            let expected = track_edges(viewport_width, &health_root, &health_track, &health_label);
            assert_eq!(
                track_edges(viewport_width, &hunger_root, &hunger_track, &hunger_label),
                expected
            );
            assert_eq!(
                track_edges(
                    viewport_width,
                    &experience_root,
                    &experience_track,
                    &experience_label,
                ),
                expected
            );
        }

        let geometry_before = (
            node::<HealthRoot>(&mut app),
            node::<HealthTrack>(&mut app),
            node::<HealthLabel>(&mut app),
            node::<HungerRoot>(&mut app),
            node::<HungerTrack>(&mut app),
            node::<HungerLabel>(&mut app),
            node::<ExperienceRoot>(&mut app),
            node::<ExperienceTrack>(&mut app),
            node::<ExperienceLabel>(&mut app),
        );
        deliver(&mut app, longest);
        assert_eq!(label(&mut app), "65535 / 65535");
        assert_eq!(
            {
                let world = app.world_mut();
                let mut query = world.query_filtered::<&Text, With<HungerLabel>>();
                query.single(world).expect("one hunger label").0.clone()
            },
            "65535 / 65535"
        );
        assert_eq!(
            {
                let world = app.world_mut();
                let mut query = world.query_filtered::<&Text, With<ExperienceLabel>>();
                query.single(world).expect("one experience label").0.clone()
            },
            "Lv 65535 · 4294967295 / 4294967295"
        );
        assert_eq!(
            geometry_before,
            (
                node::<HealthRoot>(&mut app),
                node::<HealthTrack>(&mut app),
                node::<HealthLabel>(&mut app),
                node::<HungerRoot>(&mut app),
                node::<HungerTrack>(&mut app),
                node::<HungerLabel>(&mut app),
                node::<ExperienceRoot>(&mut app),
                node::<ExperienceTrack>(&mut app),
                node::<ExperienceLabel>(&mut app),
            )
        );
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

    fn add_camera(app: &mut App, transform: Transform) {
        app.world_mut().spawn((
            WorldCamera,
            Camera {
                is_active: true,
                ..default()
            },
            Projection::Perspective(PerspectiveProjection {
                aspect_ratio: 1.0,
                ..default()
            }),
            transform,
        ));
    }

    fn hit(pos: [f32; 3]) -> MobHit {
        MobHit {
            attacker_entity_id: 41,
            attacker_pos: pos,
        }
    }

    fn pulse_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<HitPulseRoot>>();
        *query.single(world).expect("one hit pulse")
    }

    fn vignette(app: &mut App) -> (Visibility, RadialGradient) {
        let world = app.world_mut();
        let mut query =
            world.query_filtered::<(&Visibility, &BackgroundGradient), With<LowHealthVignette>>();
        let (visibility, gradient) = query.single(world).expect("one low-health vignette");
        let Gradient::Radial(gradient) = &gradient.0[0] else {
            panic!("low-health vignette is not radial");
        };
        (*visibility, gradient.clone())
    }

    fn edge_alpha(gradient: &RadialGradient) -> f32 {
        gradient
            .stops
            .last()
            .expect("one edge stop")
            .color
            .to_srgba()
            .alpha
    }

    fn transition_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<DeathTransitionRoot>>();
        *query.single(world).expect("one death transition")
    }

    fn eyelid_height(app: &mut App, wanted: Eyelid) -> Val {
        let world = app.world_mut();
        let mut query = world.query::<(&Eyelid, &Node)>();
        query
            .iter(world)
            .find_map(|(eyelid, node)| (*eyelid == wanted).then_some(node.height))
            .expect("requested eyelid")
    }

    fn add_fall(app: &mut App, elapsed: Duration) -> Entity {
        let mut fall = DeathFall::default();
        fall.advance(true, elapsed);
        app.world_mut().spawn((LocalPlayer, fall)).id()
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
    fn living_health_loss_darkens_only_the_edges_monotonically() {
        let samples = [100, 75, 50, 25, 1, 0].map(|health| vignette_alpha(vitals(health, 100)));
        assert_eq!(samples[0], 0.0, "full health has no vignette");
        assert!(samples.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            samples[4] < VIGNETTE_MAX_ALPHA,
            "one living point stays below the configured opacity bound"
        );
        assert_eq!(samples[5], VIGNETTE_MAX_ALPHA);

        let gradient = vignette_gradient(VIGNETTE_MAX_ALPHA);
        assert_eq!(gradient.stops.len(), 3);
        assert_eq!(gradient.stops[0].color.to_srgba().alpha, 0.0);
        assert_eq!(gradient.stops[1].color.to_srgba().alpha, 0.0);
        assert_eq!(
            gradient.stops[1].point,
            Val::Percent(VIGNETTE_CLEAR_PERCENT)
        );
        assert_eq!(edge_alpha(&gradient), VIGNETTE_MAX_ALPHA);
    }

    #[test]
    fn accepted_health_updates_change_the_vignette_on_the_same_frame() {
        let mut app = hud(vitals(100, 100));
        let (visibility, gradient) = vignette(&mut app);
        assert_eq!(visibility, Visibility::Hidden);
        assert_eq!(edge_alpha(&gradient), 0.0);

        deliver(&mut app, vitals(20, 100));
        let (visibility, wounded) = vignette(&mut app);
        assert_eq!(visibility, Visibility::Visible);

        deliver(&mut app, vitals(80, 100));
        let (_, healed) = vignette(&mut app);
        assert!(edge_alpha(&healed) < edge_alpha(&wounded));

        deliver(&mut app, vitals(100, 100));
        assert_eq!(vignette(&mut app).0, Visibility::Hidden);

        deliver(&mut app, dead(60));
        assert_eq!(
            vignette(&mut app).0,
            Visibility::Hidden,
            "life state, not zero health, selects the death presentation"
        );
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
    fn the_eyelids_wait_for_both_the_countdown_window_and_the_finished_fall() {
        assert!(!death_transition_due(dead(27), TICK_RATE));
        assert!(death_transition_due(dead(26), TICK_RATE));
        assert!(death_transition_due(dead(1), TICK_RATE));
        assert!(death_transition_due(dead(0), TICK_RATE));
        assert!(!death_transition_due(vitals(0, 100), TICK_RATE));

        let mut app = hud(dead(26));
        let body = add_fall(&mut app, Duration::from_millis(899));
        app.update();
        assert_eq!(death_visibility(&mut app), Visibility::Visible);
        assert_eq!(transition_visibility(&mut app), Visibility::Hidden);

        app.world_mut()
            .get_mut::<DeathFall>(body)
            .expect("local death fall")
            .advance(true, Duration::from_millis(1));
        app.update();
        assert_eq!(transition_visibility(&mut app), Visibility::Visible);
        assert_eq!(eyelid_height(&mut app, Eyelid::Upper), Val::Percent(0.0));
        assert_eq!(eyelid_height(&mut app, Eyelid::Lower), Val::Percent(0.0));
    }

    #[test]
    fn the_eyes_close_for_three_tenths_then_black_holds_until_alive() {
        let mut app = hud(dead(26));
        add_fall(&mut app, Duration::from_millis(900));
        app.update();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));

        app.update();
        let Val::Percent(partial) = eyelid_height(&mut app, Eyelid::Upper) else {
            panic!("upper eyelid height is not a percentage");
        };
        assert!(partial > 0.0 && partial < 50.0);
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<DeathTransition>().elapsed,
            Some(EYELID_CLOSE_DURATION)
        );
        assert_eq!(eyelid_height(&mut app, Eyelid::Upper), Val::Percent(50.0));
        assert_eq!(eyelid_height(&mut app, Eyelid::Lower), Val::Percent(50.0));

        let authoritative = *app.world().resource::<SelfVitals>();
        let mode = *app.world().resource::<InputMode>();
        for _ in 0..20 {
            app.update();
        }
        assert_eq!(transition_visibility(&mut app), Visibility::Visible);
        assert_eq!(eyelid_height(&mut app, Eyelid::Upper), Val::Percent(50.0));
        assert_eq!(*app.world().resource::<SelfVitals>(), authoritative);
        assert_eq!(*app.world().resource::<InputMode>(), mode);

        deliver(&mut app, vitals(100, 100));
        assert_eq!(transition_visibility(&mut app), Visibility::Hidden);
        assert_eq!(eyelid_height(&mut app, Eyelid::Upper), Val::Percent(0.0));
        assert_eq!(
            *app.world().resource::<DeathTransition>(),
            DeathTransition::default()
        );
    }

    #[test]
    fn health_presentation_never_changes_the_camera_field_of_view() {
        let mut app = hud(vitals(40, 100));
        let fov = 0.73;
        let camera = app
            .world_mut()
            .spawn((
                WorldCamera,
                Camera {
                    is_active: true,
                    ..default()
                },
                Projection::Perspective(PerspectiveProjection { fov, ..default() }),
                Transform::default(),
            ))
            .id();

        deliver(&mut app, vitals(1, 100));
        let Projection::Perspective(projection) = app
            .world()
            .get::<Projection>(camera)
            .expect("camera projection")
        else {
            panic!("camera stopped being perspective");
        };
        assert_eq!(projection.fov, fov);
    }

    #[test]
    fn death_layers_cover_the_hud_but_leave_the_pause_menu_on_top() {
        let mut app = hud(dead(27));
        add_fall(&mut app, Duration::from_millis(900));
        app.update();
        assert_eq!(death_visibility(&mut app), Visibility::Visible);
        assert_eq!(transition_visibility(&mut app), Visibility::Hidden);

        let world = app.world_mut();
        let death = *world
            .query_filtered::<&GlobalZIndex, With<DeathRoot>>()
            .single(world)
            .expect("one death overlay");
        let (transition, focus) = world
            .query_filtered::<(&GlobalZIndex, &FocusPolicy), With<DeathTransitionRoot>>()
            .single(world)
            .expect("one death transition");
        assert!(death.0 < transition.0);
        assert_eq!(*transition, GlobalZIndex(DEATH_TRANSITION_LAYER));
        assert_eq!(*focus, FocusPolicy::Pass);
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

    #[test]
    fn first_and_third_person_camera_transforms_classify_the_same_world_point() {
        let projection = Projection::Perspective(PerspectiveProjection {
            aspect_ratio: 1.0,
            ..default()
        });
        let first_person = Transform::default();
        assert!(inside_frustum(
            hit([0.0, 0.0, -5.0]),
            &projection,
            &first_person
        ));
        assert!(!inside_frustum(
            hit([10.0, 0.0, -5.0]),
            &projection,
            &first_person
        ));
        assert!(!inside_frustum(
            hit([0.0, 0.0, 5.0]),
            &projection,
            &first_person
        ));

        // A third-person camera displaced behind the player still judges against its
        // own current transform, rather than the avatar or a hard-coded view mode.
        let third_person = Transform::from_xyz(0.0, 0.0, 5.0);
        assert!(inside_frustum(
            hit([0.0, 0.0, 0.0]),
            &projection,
            &third_person
        ));
    }

    #[test]
    fn any_outside_hit_in_one_frame_starts_one_fade_and_a_later_hit_restarts_it() {
        let mut app = hud(vitals(100, 100));
        add_camera(&mut app, Transform::default());
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));

        {
            let mut inbox = app.world_mut().resource_mut::<MobHitInbox>();
            inbox.push(hit([0.0, 0.0, -5.0]));
            inbox.push(hit([10.0, 0.0, -5.0]));
            assert_eq!(inbox.pending(), 2);
        }
        app.update();
        assert_eq!(pulse_visibility(&mut app), Visibility::Visible);
        assert_eq!(app.world().resource::<MobHitInbox>().pending(), 0);
        assert_eq!(
            app.world().resource::<HitPulse>().remaining,
            HIT_PULSE_DURATION
        );

        app.update();
        assert_eq!(
            app.world().resource::<HitPulse>().remaining,
            Duration::from_millis(200)
        );
        app.world_mut()
            .resource_mut::<MobHitInbox>()
            .push(hit([0.0, 0.0, 5.0]));
        app.update();
        assert_eq!(
            app.world().resource::<HitPulse>().remaining,
            HIT_PULSE_DURATION,
            "a qualifying hit later in the fade restarts it"
        );
    }

    #[test]
    fn an_inside_hit_and_a_vitals_decrease_do_not_start_a_pulse() {
        let mut app = hud(vitals(100, 100));
        add_camera(&mut app, Transform::default());
        app.world_mut()
            .resource_mut::<MobHitInbox>()
            .push(hit([0.0, 0.0, -5.0]));
        deliver(&mut app, vitals(80, 100));

        assert_eq!(pulse_visibility(&mut app), Visibility::Hidden);
        assert_eq!(app.world().resource::<HitPulse>().remaining, Duration::ZERO);
    }

    #[test]
    fn the_pulse_fades_out_in_about_three_tenths_of_a_second() {
        let mut app = hud(vitals(100, 100));
        add_camera(&mut app, Transform::default());
        app.world_mut()
            .resource_mut::<MobHitInbox>()
            .push(hit([0.0, 0.0, 5.0]));
        app.update();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(pulse_visibility(&mut app), Visibility::Hidden);
        assert_eq!(app.world().resource::<HitPulse>().remaining, Duration::ZERO);
    }
}
