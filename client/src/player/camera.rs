//! The world camera, and the two different places its position and its direction come from.
//!
//! ## This module owns the world camera
//!
//! It moved here from `world/render.rs` when movement landed, because a camera that
//! follows a gameplay entity belongs to the module that knows where that entity is.
//! `world/render.rs` kept the terrain meshes and the one material they share.
//!
//! The camera carries the sky's clear colour, the ambient term and the distance fog as
//! components, for the plugin-ordering reason spelled out at [`spawn_camera`]. It does not
//! decide any of them: [`super::sky`] owns the curve those three are read from, and this
//! module only spawns them at the value a world with no clock keeps for ever.
//!
//! `player/hands.rs` owns one deliberate second camera for its render layer. It stays at the
//! origin so sub-block view-model offsets never make a lossy round trip through a distant f32
//! world position, draws after this camera without clearing its colour, and sees no world
//! geometry. **`IsDefaultUiCamera` is on that overlay rather than here, and the reason is the
//! whole of #808's regression**: `bevy_ui` draws its pass inside the target camera's own graph,
//! and the last camera to render is what the window ends up showing, so UI assigned to this
//! camera was drawn and then overwritten by the overlay's pass — every screen in the client went
//! black, including the login screen, while every headless test stayed green because none of them
//! renders. UI belongs on the highest-order camera on the window; `ui/status.rs` still spawns no
//! camera of its own.
//!
//! ## Position from the server, direction from here
//!
//! The camera's **translation** is the authoritative position, interpolated: the server
//! decides where the player is, and this draws that answer an eye height above their feet.
//! Nothing here corrects it, rewinds it, or replaces it with a local guess.
//!
//! Its **rotation** is the client's own look state, applied the frame the pointer moves.
//! That is not prediction and not a gameplay decision — `schemas/player.fbs` is explicit
//! that "the camera is a client concern", and the yaw the server echoes back in a snapshot
//! came from here in the first place. Waiting a tick for that echo would put the delay of
//! a network round trip on the act of looking around, which is the one thing a
//! first-person view cannot survive.
//!
//! ## Dying, in two views, decided rather than inherited
//!
//! There are two of them now, and a death does something different in each. Stated here
//! because the alternative is for it to be whatever the arithmetic happened to produce.
//!
//! - **First person: the view falls.** The camera *is* the eye, so the eye is what goes
//!   over — the pitch swings up to the sky and the eye sinks to [`DEATH_EYE_HEIGHT`], and
//!   it rests there until the server respawns the player. Nothing about the fall is
//!   reversible from here; the respawn is what ends it, and the respawn is the server's.
//! - **Third person: the view does not move at all.** The camera is an observer rather
//!   than an eye, and what falls is the *character* — `super::collapse_bodies` tips every
//!   rig the server names dead, this session's included. Tipping the camera as well would
//!   be tipping the thing that is watching, which is the one view where that is nonsense:
//!   the player is looking at their own body going down, and the camera's job is to keep
//!   it in frame. The boom, the orbit and the pitch are all untouched, so it does.
//! - **The toggle is refused while dead**, in both directions. The two views resolve a
//!   death into two different things — a camera that has fallen, and a camera watching a
//!   body that has — and flipping between them mid-death would either stand a fallen
//!   camera up or drop an upright one on its back. It is also the last playing-mode key
//!   that was not already closed by [`super::SelfVitals::dead`], and a view swap is a
//!   thing a corpse does not do.
//!
//! **None of it decides anything.** Every fall follows `EntitySnapshot.dead_players`, the
//! server's complete answer for the bodies in view; [`super::SelfVitals::dead`] only closes
//! controls and the view toggle for this session's own player. A client that skipped the
//! whole animation would be dead for the same length of time, respawn at the same moment
//! and see the same world, because no pose is ever sent back.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use std::time::Duration;

use super::constants::{
    BOOM_CLEARANCE, BOOM_LENGTH, DEATH_EYE_HEIGHT, DEATH_FALL_TIME, EYE_HEIGHT, MAX_PITCH,
    MOUNTED_BOOM_LENGTH, ORBIT_RETURN_PER_SECOND, ORBIT_SETTLED,
};
use super::sky::Daylight;
use super::target::{BlockHit, raycast};
use super::{ApplySnapshots, InputMode, LocalMount, LocalPlayer, LookState, SelfVitals};
use crate::net::{BlockCoord, Session};
use crate::world::ChunkStore;

/// Orders anything that reads where the camera is looking after the systems that aim it.
///
/// Exported because [`super::target`] casts its ray from the camera and a private system
/// function cannot be named from outside this module. A ray cast before the camera moved
/// would target what the player was looking at a frame ago, which is an outline that
/// lags the crosshair.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AimCamera;

/// Spawns the world camera and keeps it on the player.
pub struct PlayerCameraPlugin;

impl Plugin for PlayerCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewMode>()
            .init_resource::<Orbit>()
            .init_resource::<SaddlePresentation>()
            .init_resource::<LocalMount>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                // The camera follows the transforms the snapshots wrote, so it has to run
                // after they were written — a camera a frame behind the body it is
                // attached to shows the world sliding under a player who is standing
                // still.
                //
                // `toggle_view` and `settle_the_orbit` are in the chain ahead of the
                // placement for the same reason: the view the frame is drawn in is the
                // one this frame's key press asked for, not last frame's.
                (
                    toggle_view,
                    settle_the_orbit,
                    place_camera_at_spawn,
                    follow_the_player,
                )
                    .chain()
                    .in_set(AimCamera)
                    .after(ApplySnapshots)
                    // **After the input is sampled, and that ordering was missing.** The
                    // module comment above has always said the rotation is "applied the
                    // frame the pointer moves", and nothing declared it: the two sets were
                    // both `after(ApplySnapshots)` and unordered against each other, so a
                    // camera a frame behind the pointer was left to the executor's
                    // discretion. #172 made it visible rather than introducing it —
                    // `settle_the_orbit` reads the `held` flag `sample_input` writes, so an
                    // undeclared order is a return animation that starts a frame late or
                    // not, depending on the schedule.
                    .after(super::sample_input),
            );
    }
}

/// Marks the camera this module owns.
#[derive(Component)]
pub struct WorldCamera;

fn spawn_camera(mut commands: Commands) {
    commands.spawn((
        WorldCamera,
        Camera3d::default(),
        // The sky and the ambient light are set on the camera rather than through
        // `ClearColor` and `GlobalAmbientLight`, so they do not depend on this plugin
        // being built after the one that inserts those resources' defaults.
        //
        // They start at the fixed sky and stay there for a world whose server keeps no
        // clock. `player/sky.rs` is the only other writer, and it writes only once a
        // `ServerWelcome` has declared a day length — see the module comment there.
        Camera {
            clear_color: ClearColorConfig::Custom(Daylight::FIXED.sky),
            ..default()
        },
        AmbientLight {
            brightness: Daylight::FIXED.ambient_brightness,
            ..default()
        },
        // Explicit, and load-bearing. `Camera3d`'s default tonemapper is `TonyMcMapface`,
        // which reads a KTX2 lookup texture that only the `tonemapping_luts` feature
        // ships — without it Bevy logs an error per pipeline and renders through a
        // placeholder. `AcesFitted` is computed in the shader, so the client needs no LUT,
        // and therefore no `ktx2` and no `zstd` in its dependency graph. See the feature
        // comment in Cargo.toml.
        Tonemapping::AcesFitted,
        Transform::default(),
    ));
}

/// Puts the camera at the spawn point until the first snapshot arrives.
///
/// One tick of terrain rather than one tick of black: the welcome says where the server
/// has placed the player, and the first snapshot that will say it again is a tick away.
/// It also covers a server that welcomes and then goes quiet — the player sees the world
/// they cannot move in, which is a great deal more diagnosable than an empty screen.
///
/// Runs only when the session resource changes, which happens once.
fn place_camera_at_spawn(
    session: Option<Res<Session>>,
    mut cameras: Query<&mut Transform, With<WorldCamera>>,
) {
    let Some(session) = session else {
        return;
    };
    if !session.is_changed() {
        return;
    }

    let [x, y, z] = session.0.spawn;
    for mut transform in &mut cameras {
        transform.translation = Vec3::new(x, y + EYE_HEIGHT, z);
    }
}

/// Everything that decides where the camera goes and which way it points.
///
/// One `SystemParam` rather than four resources threaded through a signature, for the
/// reason [`super::InputGate`] is one: it gives "the state a placement is computed from" a
/// name, and it keeps [`follow_the_player`] inside the argument budget a fourth resource
/// took it past.
#[derive(SystemParam)]
struct Aim<'w> {
    look: Res<'w, LookState>,
    orbit: Res<'w, Orbit>,
    view: Res<'w, ViewMode>,
    time: Res<'w, Time>,
    mount: Res<'w, LocalMount>,
    saddle: ResMut<'w, SaddlePresentation>,
}

/// What the camera is currently drawing of the walk-to-saddle change: the height the eye
/// sits at, and how far back the third-person boom reaches, each on its way between two
/// exact values.
///
/// The target is an authoritative binary answer in [`LocalMount`]; this resource is only
/// the cosmetic path between the two states. It cannot move the player or keep the saddle
/// composition alive after the server removes the mount entry. The two fields advance
/// together, from the same answer on the same clock, so the boom can never be at the saddle
/// while the eye is still walking.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
struct SaddlePresentation {
    eye_height: f32,
    boom: f32,
}

impl Default for SaddlePresentation {
    fn default() -> Self {
        Self {
            eye_height: EYE_HEIGHT,
            boom: BOOM_LENGTH,
        }
    }
}

impl SaddlePresentation {
    /// Where both values rest once the transition onto the horse has finished.
    #[cfg(test)]
    const MOUNTED: Self = Self {
        eye_height: SADDLE_EYE_HEIGHT,
        boom: MOUNTED_BOOM_LENGTH,
    };

    /// One frame further towards whichever state `mounted` names.
    fn advanced(self, mounted: bool, delta: Duration) -> Self {
        Self {
            eye_height: next_eye_height(self.eye_height, mounted, delta),
            boom: next_boom_length(self.boom, mounted, delta),
        }
    }
}

/// The walking eye lifted by exactly what lifts the rider's rig onto the seat. Derived, so
/// the horse's proportions move it without an edit here; `saddle.rs` reads it too.
pub(super) const SADDLE_EYE_HEIGHT: f32 = EYE_HEIGHT + super::horse::RIDER_LIFT;
const EYE_TRANSITION_TIME: Duration = Duration::from_millis(260);

/// One frame of a transition between two exact values, `walking` and `saddle`, towards
/// whichever `mounted` names: a constant rate that crosses the span in
/// [`EYE_TRANSITION_TIME`], clamped so the value *arrives* at exactly the endpoint. The eye
/// height and the boom are the same shape with different endpoints — one function, two calls.
fn next_between(current: f32, walking: f32, saddle: f32, mounted: bool, delta: Duration) -> f32 {
    let target = if mounted { saddle } else { walking };
    let step = (saddle - walking) * delta.as_secs_f32() / EYE_TRANSITION_TIME.as_secs_f32();
    if current < target {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
}

fn next_eye_height(current: f32, mounted: bool, delta: Duration) -> f32 {
    next_between(current, EYE_HEIGHT, SADDLE_EYE_HEIGHT, mounted, delta)
}

fn next_boom_length(current: f32, mounted: bool, delta: Duration) -> f32 {
    next_between(current, BOOM_LENGTH, MOUNTED_BOOM_LENGTH, mounted, delta)
}

/// Keeps the camera at the player's eyes, looking where the player is looking.
///
/// The two halves come from different places on purpose — see the module comment. The
/// query is filtered `Without<LocalPlayer>` because Bevy cannot otherwise prove that the
/// camera's `Transform` and the player's are different components of different entities,
/// and would refuse the system rather than risk aliasing them.
fn follow_the_player(
    mut aim: Aim<'_>,
    session: Option<Res<Session>>,
    store: Option<Res<ChunkStore>>,
    player: Query<(&Transform, &DeathFall), With<LocalPlayer>>,
    mut cameras: Query<&mut Transform, (With<WorldCamera>, Without<LocalPlayer>)>,
) {
    let Some((feet, fallen)) = player
        .iter()
        .next()
        .map(|(transform, fall)| (transform.translation, fall.fallen()))
    else {
        // No snapshot has named this session's own entity yet. The spawn placement above
        // is what the player is looking at until one does.
        return;
    };

    // Both or neither: the chunk size is what turns a voxel coordinate into a chunk, so a
    // store with no session to size it is a store nothing can be looked up in — and an
    // unstreamed world is not solid, which is the same answer the aiming ray gets.
    let solid = session
        .as_deref()
        .zip(store.as_deref())
        .map(|(session, store)| (store, usize::from(session.0.chunk_size)));
    let next = aim.saddle.advanced(aim.mount.mounted(), aim.time.delta());
    if *aim.saddle != next {
        *aim.saddle = next;
    }
    let placed = camera_placement_with_saddle(
        feet,
        *aim.look,
        *aim.orbit,
        *aim.view,
        *aim.saddle,
        fallen,
        |voxel| {
            solid.is_some_and(|(store, size)| {
                store.solid_at(
                    BlockCoord {
                        x: voxel.x,
                        y: voxel.y,
                        z: voxel.z,
                    },
                    size,
                )
            })
        },
    );
    for mut transform in &mut cameras {
        *transform = placed;
    }
}

// ---------------------------------------------------------------------------
// Which view, and the angle that is only the camera's
// ---------------------------------------------------------------------------

/// The key that swaps the two views.
///
/// F5 because that is the key this genre has trained everybody to reach for, and a view
/// toggle nobody finds is a feature nobody has.
const TOGGLE: KeyCode = KeyCode::F5;

/// Which view the world is drawn in.
///
/// **A way of looking, not a way of playing** — see #172. First person is what the game
/// is played in and is unchanged; third person is a mode to switch into, look at the
/// character somebody made, and switch back out of. Everything that follows from that is
/// a subtraction: no crosshair, no outline, no request.
///
/// Nothing about it crosses the wire. `schemas/player.fbs` says the camera is a client
/// concern, and the server cannot tell which view a client is in.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// The camera is the eye. What the game is played in.
    #[default]
    FirstPerson,
    /// The camera is behind the character, and aiming is off.
    ThirdPerson,
}

impl ViewMode {
    /// Whether the camera is the eye this frame.
    ///
    /// The question every other module asks, phrased as the affirmative one: the gates
    /// and the crosshair are *on* in first person, and third person is what removes them.
    pub const fn first_person(self) -> bool {
        matches!(self, Self::FirstPerson)
    }
}

/// The angle that belongs to the camera and not to the character.
///
/// An **offset** from [`LookState`] rather than an absolute direction, which is what
/// makes the two acceptance criteria fall out instead of being arranged. At rest it is
/// zero, so the camera is exactly behind the character and stays there while they turn.
/// Holding the orbit key moves this and never `LookState`, so the facing the server reads
/// does not change. Releasing it animates this back to zero, which *is* the camera
/// returning behind the character — including while the character is turning, which a
/// remembered absolute angle would have had to chase.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct Orbit {
    /// Added to `LookState::yaw` before the camera is rotated.
    pub yaw: f32,
    /// Added to `LookState::pitch`, and the sum is clamped — not this.
    pub pitch: f32,
    /// Whether the orbit key is held this frame. `settle_the_orbit` reads it and nothing
    /// else does; it is on the resource rather than re-read from the keyboard so that the
    /// one place deciding what the mouse moves is `sample_input`.
    pub held: bool,
}

impl Orbit {
    /// Whether the camera is anywhere but directly behind the character.
    pub fn swung(self) -> bool {
        self.yaw != 0.0 || self.pitch != 0.0
    }
}

/// Flips the view, and puts the camera back behind the character when it does.
///
/// The orbit is cleared rather than animated on a toggle: the animation exists so that
/// releasing the orbit key does not snap, and a player leaving the view entirely has
/// nothing on screen for it to be smooth *for*.
///
/// **Refused while the server says the player is dead**, which is the module comment's
/// third decision and the only one that is a *refusal* rather than a pose. The two views
/// resolve a death differently — this camera falls, the other one watches a body fall — so
/// a swap part way through either would have to stand a fallen camera up or drop an upright
/// one on its back, and neither is an animation anybody asked for. `SelfVitals::dead` is
/// the same gate every other playing control already reads; this key was the last one that
/// did not, and it decides nothing either way — the server refuses a dead player's
/// requests whichever view they are in.
fn toggle_view(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mode: Res<InputMode>,
    vitals: Res<SelfVitals>,
    mut view: ResMut<ViewMode>,
    mut orbit: ResMut<Orbit>,
) {
    // Optional for the reason `sample_input` gives: this module's own tests build an app
    // with no `InputPlugin`, and absent input is no input rather than a panic.
    let Some(keys) = keys else {
        return;
    };
    // A mode transition and the key that caused it share a frame — the same rule
    // `InputGate::may_act` keeps, and for the same reason: `Escape` closing the pause menu
    // must not also be read as something else.
    if *mode != InputMode::Playing || mode.is_changed() || !keys.just_pressed(TOGGLE) {
        return;
    }
    if vitals.dead() {
        return;
    }

    *view = match *view {
        ViewMode::FirstPerson => ViewMode::ThirdPerson,
        ViewMode::ThirdPerson => ViewMode::FirstPerson,
    };
    if orbit.swung() {
        *orbit = Orbit {
            held: orbit.held,
            ..Orbit::default()
        };
    }
}

/// Swings the camera back behind the character once the orbit is released.
///
/// Exponential, so the return is fast where the angle is large and gentle where it is
/// small — and snapped inside [`ORBIT_SETTLED`], which is what makes it an animation that
/// *ends*. A decay alone approaches zero and never arrives, and "the camera is nearly
/// behind the player" is a state with no reason to persist for the rest of a session.
fn settle_the_orbit(time: Res<Time>, mut orbit: ResMut<Orbit>) {
    if orbit.held || !orbit.swung() {
        return;
    }

    let remaining = ORBIT_RETURN_PER_SECOND.powf(time.delta_secs());
    let next = Orbit {
        yaw: orbit.yaw * remaining,
        pitch: orbit.pitch * remaining,
        held: orbit.held,
    };
    let settled = next.yaw.abs() < ORBIT_SETTLED && next.pitch.abs() < ORBIT_SETTLED;
    *orbit = if settled {
        Orbit {
            held: orbit.held,
            ..Orbit::default()
        }
    } else {
        next
    };
}

// ---------------------------------------------------------------------------
// Going over
// ---------------------------------------------------------------------------

/// How long one player body has been lying where it died, or `None` while the server says
/// it is alive.
///
/// One component per body, driven by `EntitySnapshot.dead_players` in
/// `super::collapse_bodies`. The local body's component also drives the first-person camera,
/// so the body seen in third person and the eye seen in first person cannot run on two clocks.
/// A respawn clears it in one assignment, so nothing has to animate the view back upright;
/// the server has already moved the player somewhere else.
///
/// Nothing reads it as a fact. It feeds a pose and a rig rotation and reaches no request,
/// no snapshot and no decision.
#[derive(Component, Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct DeathFall(Option<Duration>);

impl DeathFall {
    /// The pose of a body entering view for the first time.
    ///
    /// An already-dead body starts on the ground rather than replaying an event the viewer
    /// was not present for. A living body starts upright. Later transitions are advanced by
    /// [`Self::advance`].
    pub(super) fn newly_seen(dead: bool) -> Self {
        Self(dead.then_some(DEATH_FALL_TIME))
    }

    /// Advances or clears the presentation according to the newest server snapshot.
    pub(crate) fn advance(&mut self, dead: bool, delta: Duration) {
        self.0 = if dead {
            Some(self.0.unwrap_or(Duration::ZERO) + delta)
        } else {
            None
        };
    }

    /// How far over the view has gone, from nought to one.
    ///
    /// Squared, so it accelerates, and clamped, so it *ends* — the same curve a mob's fall
    /// uses, which is deliberate: a player watching their own body go down in third person
    /// is watching two falls, and two curves would be visible as one lagging the other.
    pub(crate) fn fallen(self) -> f32 {
        let Some(elapsed) = self.0 else {
            return 0.0;
        };
        let progress = (elapsed.as_secs_f32() / DEATH_FALL_TIME.as_secs_f32()).clamp(0.0, 1.0);
        progress * progress
    }

    /// Whether the presentation has reached the pose its camera and body will hold.
    pub(crate) fn finished(self) -> bool {
        self.fallen() >= 1.0
    }
}

/// Where the camera sits and what it looks at, in whichever view is current.
///
/// The eye is the same point in both: the character's feet plus the current walking-to-
/// saddle presentation height. First person puts the camera there. Third person aims from
/// there and then walks backwards along the view direction by the presentation's boom,
/// which is why the two share a function rather than being two systems that have to agree
/// about what an eye is.
///
/// **`fallen` is the one input the two views do not share.** In first person the camera is
/// the eye, so the eye is what goes over; in third person it is an observer watching a body
/// go over, and an observer that fell with it would take the thing being watched out of
/// frame. See the module comment, where the decision is argued rather than only applied.
#[cfg(test)]
fn camera_placement(
    feet: Vec3,
    look: LookState,
    orbit: Orbit,
    view: ViewMode,
    fallen: f32,
    solid: impl FnMut(IVec3) -> bool,
) -> Transform {
    camera_placement_with_saddle(
        feet,
        look,
        orbit,
        view,
        SaddlePresentation::default(),
        fallen,
        solid,
    )
}

fn camera_placement_with_saddle(
    feet: Vec3,
    look: LookState,
    orbit: Orbit,
    view: ViewMode,
    saddle: SaddlePresentation,
    fallen: f32,
    solid: impl FnMut(IVec3) -> bool,
) -> Transform {
    let eye_height = saddle.eye_height;
    // Yaw about the world's up axis, then pitch about the camera's own right — in that
    // order, which is what keeps the horizon level. The other order rolls the view as soon
    // as both are non-zero.
    //
    // The orbit is added before the clamp rather than after: a player who has swung the
    // camera fully up should stop at the same place a player who looked fully up does,
    // and clamping the two separately would allow twice the pitch between them.
    let aimed = (look.pitch + orbit.pitch).clamp(-MAX_PITCH, MAX_PITCH);

    if view.first_person() {
        // Going over backwards *is* the view swinging up: the player ends on their back
        // looking at the sky, so the pitch travels to the top of its own range and the eye
        // sinks to the ground. Interpolated from wherever the player happened to be looking
        // when they died rather than snapped, so the last thing they saw slides out of view.
        //
        // `MAX_PITCH` rather than a right angle, because straight up is the degenerate
        // direction this whole file avoids: at exactly ±π/2 every yaw looks the same and
        // the image flips as the pitch crosses it.
        let pitch = aimed + (MAX_PITCH - aimed) * fallen;
        return Transform {
            translation: feet + Vec3::Y * (eye_height + (DEATH_EYE_HEIGHT - eye_height) * fallen),
            rotation: Quat::from_rotation_y(look.yaw + orbit.yaw) * Quat::from_rotation_x(pitch),
            ..default()
        };
    }

    // Third person, where `fallen` is deliberately unread: what falls is the character, and
    // this camera's job for the next three seconds is to keep it in frame.
    let eye = feet + Vec3::Y * eye_height;
    let rotation = Quat::from_rotation_y(look.yaw + orbit.yaw) * Quat::from_rotation_x(aimed);

    // The camera's own backwards, normalised: `face_distance` divides by a component
    // of it, so it has to be a unit vector for the quotient to be a length in blocks.
    let back = -(rotation * Vec3::NEG_Z).normalize_or_zero();
    Transform {
        translation: eye + back * boom_length(eye, back, saddle.boom, solid),
        rotation,
        ..default()
    }
}

/// How far back the boom actually reaches, given what is behind the player.
///
/// The camera stops at the first solid voxel between the eyes and where it would
/// otherwise sit, so standing against a wall does not put the view inside it. This is
/// presentation and decides nothing: it reads the world this client has been streamed and
/// never asks the server anything, and a chunk that has not arrived is not solid — the
/// same answer `ChunkStore::solid_at` gives the aiming ray, and honest for the same
/// reason.
///
/// `reach` is how far the boom would go with nothing behind the player — [`BOOM_LENGTH`],
/// [`MOUNTED_BOOM_LENGTH`], or the presentation's way between them. The rule is the same
/// whichever it is: the ray, the face it hits, and [`BOOM_CLEARANCE`] in front of that.
///
/// Takes the predicate rather than the store, which is the shape [`raycast`] itself has
/// and for the same reason: what a boom needs to know is whether a voxel stops it, and a
/// test that has to assemble a chunk store to say "there is a wall here" is a test about
/// chunk stores.
fn boom_length(eye: Vec3, back: Vec3, reach: f32, solid: impl FnMut(IVec3) -> bool) -> f32 {
    let Some(hit) = raycast(eye, back, reach, solid) else {
        return reach;
    };

    // Pulled forward off the face it hit, and never through the eye: a player wedged into
    // a corner gets a camera at their own eyes rather than one behind their forehead.
    (face_distance(eye, back, reach, hit) - BOOM_CLEARANCE).max(0.0)
}

/// How far along `direction` the eye is from the face the ray entered through.
///
/// [`BlockHit`] names the voxel and the face and does not carry a distance — the aiming
/// ray never needed one, because what it reports is *which block*. A boom needs the
/// length, so it is recovered from the two: the face is an outward unit axis, so the plane
/// it lies in is the block's coordinate on that axis plus one for a positive face, and the
/// distance is how far along the ray that plane is.
fn face_distance(eye: Vec3, direction: Vec3, reach: f32, hit: BlockHit) -> f32 {
    let Some(axis) = (0..3).find(|axis| hit.face[*axis] != 0) else {
        // A zero face means the eye is already inside a solid voxel — see [`BlockHit`].
        // The camera goes nowhere, which is the same answer as being flush against a wall
        // and the only one that does not put the view further inside the rock.
        return 0.0;
    };

    let plane = hit.block[axis] as f32 + f32::from(hit.face[axis] > 0);
    let component = direction[axis];
    if component == 0.0 {
        // Unreachable: a face is crossed on the axis the ray is moving along, so the
        // component that named it cannot be zero. Answered rather than divided by, because
        // a camera is the last thing that should produce a NaN translation.
        return reach;
    }
    ((plane - eye[axis]) / component).clamp(0.0, reach)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Looking along -Z with no orbit, which is where a fresh [`LookState`] points.
    fn looking_ahead() -> (LookState, Orbit) {
        (LookState::default(), Orbit::default())
    }

    /// A player who has not fallen over, spelled once so that every test that is not about
    /// a death says so rather than passing a bare zero.
    const UPRIGHT: f32 = 0.0;

    /// A player whose fall has finished.
    const FALLEN: f32 = 1.0;

    #[test]
    fn mount_and_dismount_move_the_eye_smoothly_between_exact_heights() {
        let rising = next_eye_height(EYE_HEIGHT, true, EYE_TRANSITION_TIME / 2);
        assert!(rising > EYE_HEIGHT && rising < SADDLE_EYE_HEIGHT);
        assert_eq!(
            next_eye_height(rising, true, EYE_TRANSITION_TIME),
            SADDLE_EYE_HEIGHT
        );

        let falling = next_eye_height(SADDLE_EYE_HEIGHT, false, EYE_TRANSITION_TIME / 2);
        assert!(falling > EYE_HEIGHT && falling < SADDLE_EYE_HEIGHT);
        assert_eq!(
            next_eye_height(falling, false, EYE_TRANSITION_TIME),
            EYE_HEIGHT
        );

        let (look, orbit) = looking_ahead();
        let feet = Vec3::new(1.5, 64.0, -2.5);
        let mounted = camera_placement_with_saddle(
            feet,
            look,
            orbit,
            ViewMode::FirstPerson,
            SaddlePresentation::MOUNTED,
            UPRIGHT,
            |_| false,
        );
        assert_eq!(mounted.translation, feet + Vec3::Y * SADDLE_EYE_HEIGHT);
    }

    /// The third-person boom backs off onto the horse and closes up on dismount between two
    /// exact lengths, on the eye height's clock — the mirror of the test above.
    #[test]
    fn mount_and_dismount_back_the_boom_off_between_exact_lengths() {
        let backing = next_boom_length(BOOM_LENGTH, true, EYE_TRANSITION_TIME / 2);
        assert!(backing > BOOM_LENGTH && backing < MOUNTED_BOOM_LENGTH);
        assert_eq!(
            next_boom_length(backing, true, EYE_TRANSITION_TIME),
            MOUNTED_BOOM_LENGTH
        );

        let closing = next_boom_length(MOUNTED_BOOM_LENGTH, false, EYE_TRANSITION_TIME / 2);
        assert!(closing > BOOM_LENGTH && closing < MOUNTED_BOOM_LENGTH);
        assert_eq!(
            next_boom_length(closing, false, EYE_TRANSITION_TIME),
            BOOM_LENGTH
        );

        // One clock: half way through the transition the boom is exactly as far along its
        // span as the eye is along its own, and both arrive together.
        let half = SaddlePresentation::default().advanced(true, EYE_TRANSITION_TIME / 2);
        let eye_along = (half.eye_height - EYE_HEIGHT) / (SADDLE_EYE_HEIGHT - EYE_HEIGHT);
        let boom_along = (half.boom - BOOM_LENGTH) / (MOUNTED_BOOM_LENGTH - BOOM_LENGTH);
        assert!(
            (eye_along - boom_along).abs() < 1e-5,
            "the eye was {eye_along} of the way and the boom {boom_along}"
        );
        assert_eq!(
            half.advanced(true, EYE_TRANSITION_TIME),
            SaddlePresentation::MOUNTED
        );
        assert_eq!(
            SaddlePresentation::MOUNTED.advanced(false, EYE_TRANSITION_TIME),
            SaddlePresentation::default()
        );

        // Mounted, in third person, with nothing streamed to stop it: the camera sits the
        // whole mounted boom behind the saddle eye.
        let (look, orbit) = looking_ahead();
        let feet = Vec3::new(1.5, 64.0, -2.5);
        let eye = feet + Vec3::Y * SADDLE_EYE_HEIGHT;
        let placed = camera_placement_with_saddle(
            feet,
            look,
            orbit,
            ViewMode::ThirdPerson,
            SaddlePresentation::MOUNTED,
            UPRIGHT,
            |_| false,
        );
        assert!(
            (placed.translation - (eye + Vec3::Z * MOUNTED_BOOM_LENGTH)).length() < 1e-4,
            "the mounted boom put the camera at {}",
            placed.translation
        );

        // And the obstruction rule is the one the walking boom has: the same wall 2.5
        // blocks back stops the longer boom at the same clearance in front of it.
        let feet = Vec3::new(0.5, 64.0, -2.5);
        let eye = feet + Vec3::Y * SADDLE_EYE_HEIGHT;
        let stopped = camera_placement_with_saddle(
            feet,
            look,
            orbit,
            ViewMode::ThirdPerson,
            SaddlePresentation::MOUNTED,
            UPRIGHT,
            |voxel| voxel.z >= 0,
        );
        let travelled = stopped.translation.z - eye.z;
        assert!(
            (travelled - (2.5 - BOOM_CLEARANCE)).abs() < 1e-4,
            "the mounted boom travelled {travelled} towards a wall 2.5 blocks away"
        );
    }

    #[test]
    fn first_person_puts_the_camera_at_the_eye_whatever_is_behind_the_player() {
        let (look, orbit) = looking_ahead();
        let feet = Vec3::new(1.5, 64.0, -2.5);
        // Solid everywhere: the boom would be cut to nothing, and first person never asks.
        let placed = camera_placement(feet, look, orbit, ViewMode::FirstPerson, UPRIGHT, |_| true);
        assert_eq!(placed.translation, feet + Vec3::Y * EYE_HEIGHT);
    }

    #[test]
    fn the_boom_stops_in_front_of_the_wall_behind_the_player() {
        // **The criterion this exists for**: a player with their back to a wall is looking
        // at their own back, not through the wall at whatever is on the far side of it.
        //
        // Looking along -Z, so the boom goes towards +Z. The eye is at z = -2.5, and the
        // voxel at z = 0 is solid: its near face is the plane z = 0, which is 2.5 blocks
        // back — well inside the 4-block boom.
        let (look, orbit) = looking_ahead();
        let feet = Vec3::new(0.5, 64.0, -2.5);
        let eye = feet + Vec3::Y * EYE_HEIGHT;
        let placed = camera_placement(feet, look, orbit, ViewMode::ThirdPerson, UPRIGHT, |voxel| {
            voxel.z >= 0
        });

        let travelled = placed.translation.z - eye.z;
        assert!(
            (travelled - (2.5 - BOOM_CLEARANCE)).abs() < 1e-4,
            "the boom travelled {travelled} towards a wall 2.5 blocks away"
        );
        assert!(
            placed.translation.z < 0.0,
            "the camera ended up inside the wall at {}",
            placed.translation.z
        );
    }

    #[test]
    fn an_unstreamed_world_stops_nothing() {
        // A chunk that has not arrived is not solid — the same answer `solid_at` gives the
        // aiming ray, and honest for the same reason: this client knows nothing about it.
        let (look, orbit) = looking_ahead();
        let feet = Vec3::new(0.5, 64.0, -2.5);
        let eye = feet + Vec3::Y * EYE_HEIGHT;
        let placed = camera_placement(feet, look, orbit, ViewMode::ThirdPerson, UPRIGHT, |_| false);
        assert!((placed.translation.z - (eye.z + BOOM_LENGTH)).abs() < 1e-4);
    }

    #[test]
    fn a_player_inside_a_solid_voxel_gets_a_camera_at_their_own_eyes() {
        // The one case `BlockHit` reports with a zero face: the ray started inside a solid
        // voxel, so there is no face it entered through. The camera goes nowhere, which is
        // the only answer that does not put the view further into the rock.
        let (look, orbit) = looking_ahead();
        let feet = Vec3::new(0.5, 64.0, -2.5);
        let eye = feet + Vec3::Y * EYE_HEIGHT;
        let placed = camera_placement(feet, look, orbit, ViewMode::ThirdPerson, UPRIGHT, |_| true);
        assert_eq!(placed.translation, eye);
    }

    /// **First person: the view falls backwards and comes to rest on the sky.**
    ///
    /// Two things at once, and both are the criterion rather than decoration: the pitch
    /// ends at the top of its range, which is the "falls backwards" the issue asks for read
    /// as what a first-person camera can actually do, and the eye ends on the ground, which
    /// is what tells it apart from a player who merely looked up.
    #[test]
    fn dying_in_first_person_lays_the_view_on_its_back() {
        let (look, orbit) = looking_ahead();
        let feet = Vec3::new(0.5, 64.0, 0.5);

        let alive = camera_placement(feet, look, orbit, ViewMode::FirstPerson, UPRIGHT, |_| false);
        assert_eq!(alive.translation, feet + Vec3::Y * EYE_HEIGHT);

        let dead = camera_placement(feet, look, orbit, ViewMode::FirstPerson, FALLEN, |_| false);
        assert!(
            (dead.translation.y - (feet.y + DEATH_EYE_HEIGHT)).abs() < 1e-5,
            "the eye came to rest {} above the feet, want {DEATH_EYE_HEIGHT}",
            dead.translation.y - feet.y
        );

        // Looking at the sky: the view direction is very nearly straight up, and it is
        // `MAX_PITCH` rather than a right angle because straight up is degenerate.
        let looking = dead.rotation * Vec3::NEG_Z;
        assert!(
            looking.y > MAX_PITCH.sin() - 1e-5,
            "a fallen player is looking at {looking} rather than at the sky"
        );

        // And it is an interpolation from wherever they were looking rather than a snap:
        // half way over is between the two.
        let midway = camera_placement(feet, look, orbit, ViewMode::FirstPerson, 0.5, |_| false);
        let part_way = midway.rotation * Vec3::NEG_Z;
        assert!(
            part_way.y > 0.0 && part_way.y < looking.y,
            "the fall jumped straight to the sky: {part_way}"
        );
    }

    /// **Third person: the camera does not move, because what falls is the body.**
    ///
    /// The decision the issue asked to be made deliberately, pinned so that it stays made.
    /// A camera that tipped here would take the character out of frame at exactly the
    /// moment the player is watching them go down, which is the case this view is most
    /// worth having for.
    #[test]
    fn dying_in_third_person_leaves_the_camera_where_it_was_watching_from() {
        let (look, orbit) = looking_ahead();
        let feet = Vec3::new(0.5, 64.0, 0.5);

        let alive = camera_placement(feet, look, orbit, ViewMode::ThirdPerson, UPRIGHT, |_| false);
        let dead = camera_placement(feet, look, orbit, ViewMode::ThirdPerson, FALLEN, |_| false);

        assert_eq!(
            alive.translation, dead.translation,
            "the third-person camera moved when the player died"
        );
        assert_eq!(
            alive.rotation, dead.rotation,
            "the third-person camera tipped over with the body it is watching"
        );
    }

    /// The view toggle is refused while the server says the player is dead, and works
    /// again once it says otherwise.
    ///
    /// Driven through the system rather than by asserting the branch, because what is worth
    /// holding is that the *key* does nothing — and the key is read in a system with four
    /// conditions on it, three of which were already there.
    #[test]
    fn the_view_cannot_be_swapped_while_dead() {
        use crate::net::{LifeState, PlayerVitals};

        fn view_after_f5(life_state: LifeState) -> ViewMode {
            let mut keys = ButtonInput::default();
            keys.press(TOGGLE);

            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .init_resource::<InputMode>()
                .init_resource::<ViewMode>()
                .init_resource::<Orbit>()
                .insert_resource(SelfVitals::from_server(PlayerVitals {
                    health: if life_state == LifeState::Dead { 0 } else { 60 },
                    max_health: 60,
                    hunger: 100,
                    max_hunger: 100,
                    level: 1,
                    experience: 0,
                    experience_to_next: 50,
                    life_state,
                    respawn_ticks: 0,
                    invulnerable: false,
                    blocking: false,
                }))
                .add_systems(Update, toggle_view);

            // One update before the key arrives, to spend the `is_changed` flag a freshly
            // inserted `InputMode` carries — which `toggle_view` reads as "a mode
            // transition shares this frame" and refuses on, for every life state. No
            // `InputPlugin`, so nothing clears the press between the two.
            app.update();
            app.insert_resource(keys);
            app.update();
            *app.world().resource::<ViewMode>()
        }

        assert_eq!(
            view_after_f5(LifeState::Dead),
            ViewMode::FirstPerson,
            "a dead player swapped the view"
        );
        assert_eq!(
            view_after_f5(LifeState::Alive),
            ViewMode::ThirdPerson,
            "the toggle was refused to a living player, so the test above proved nothing"
        );
    }

    /// The curve exists exactly while the newest server state says the body is dead, and a
    /// respawn clears it in one assignment rather than animating anything back.
    #[test]
    fn the_fall_curve_stops_and_clears_with_server_state() {
        assert_eq!(DeathFall::default().fallen(), 0.0);
        assert_eq!(DeathFall(Some(Duration::ZERO)).fallen(), 0.0);
        assert_eq!(DeathFall(Some(DEATH_FALL_TIME)).fallen(), 1.0);
        assert_eq!(
            DeathFall(Some(DEATH_FALL_TIME * 10)).fallen(),
            1.0,
            "the view kept going over past the end of its own fall"
        );

        // Accelerating, and the same curve a mob's fall uses: in third person the player
        // watches both at once, and two curves would show as one lagging the other.
        let halfway = DeathFall(Some(DEATH_FALL_TIME / 2)).fallen();
        assert!(
            halfway < 0.5,
            "the fall was half over at half the time ({halfway}), so it does not accelerate"
        );

        let mut fall = DeathFall::default();
        fall.advance(true, DEATH_FALL_TIME);
        assert_eq!(fall.fallen(), 1.0);
        fall.advance(false, Duration::ZERO);
        assert_eq!(fall, DeathFall::default(), "a respawn left the fall behind");

        assert_eq!(
            DeathFall::newly_seen(true).fallen(),
            1.0,
            "a body first seen dead replayed its fall from standing"
        );
    }

    #[test]
    fn the_orbit_and_the_look_share_one_pitch_clamp() {
        // Clamped on the sum, not on each: a player who has swung the camera fully up and
        // then looks up stops where either alone would, rather than at twice the angle.
        let look = LookState {
            yaw: 0.0,
            pitch: MAX_PITCH,
        };
        let orbit = Orbit {
            yaw: 0.0,
            pitch: MAX_PITCH,
            held: true,
        };
        let placed = camera_placement(
            Vec3::ZERO,
            look,
            orbit,
            ViewMode::FirstPerson,
            UPRIGHT,
            |_| false,
        );
        let only_look = camera_placement(
            Vec3::ZERO,
            look,
            Orbit::default(),
            ViewMode::FirstPerson,
            UPRIGHT,
            |_| false,
        );
        assert!(placed.rotation.angle_between(only_look.rotation) < 1e-5);
    }
}
