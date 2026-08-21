//! The one camera, and the two different places its position and its direction come from.
//!
//! ## This module owns the one camera
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
//! There is still exactly one camera, and that is a rule rather than a coincidence. Two
//! cameras targeting one window need explicit ordering and clear-colour configuration to
//! stop one erasing the other, and `bevy_ui` renders in the 3D graph as readily as the 2D
//! one — so the status text draws through this camera and `ui/status.rs` spawns none of
//! its own.
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

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;

use super::constants::EYE_HEIGHT;
use super::sky::Daylight;
use super::{ApplySnapshots, LocalPlayer, LookState};
use crate::net::Session;

/// Orders anything that reads where the camera is looking after the systems that aim it.
///
/// Exported because [`super::target`] casts its ray from the camera and a private system
/// function cannot be named from outside this module. A ray cast before the camera moved
/// would target what the player was looking at a frame ago, which is an outline that
/// lags the crosshair.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AimCamera;

/// Spawns the one camera and keeps it on the player.
pub struct PlayerCameraPlugin;

impl Plugin for PlayerCameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_camera).add_systems(
            Update,
            // The camera follows the transforms the snapshots wrote, so it has to run
            // after they were written — a camera a frame behind the body it is attached to
            // shows the world sliding under a player who is standing still.
            (place_camera_at_spawn, follow_the_player)
                .chain()
                .in_set(AimCamera)
                .after(ApplySnapshots),
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

/// Keeps the camera at the player's eyes, looking where the player is looking.
///
/// The two halves come from different places on purpose — see the module comment. The
/// query is filtered `Without<LocalPlayer>` because Bevy cannot otherwise prove that the
/// camera's `Transform` and the player's are different components of different entities,
/// and would refuse the system rather than risk aliasing them.
fn follow_the_player(
    look: Res<LookState>,
    player: Query<&Transform, With<LocalPlayer>>,
    mut cameras: Query<&mut Transform, (With<WorldCamera>, Without<LocalPlayer>)>,
) {
    let Some(feet) = player.iter().next().map(|transform| transform.translation) else {
        // No snapshot has named this session's own entity yet. The spawn placement above
        // is what the player is looking at until one does.
        return;
    };

    for mut transform in &mut cameras {
        transform.translation = feet + Vec3::Y * EYE_HEIGHT;
        // Yaw about the world's up axis, then pitch about the camera's own right — in that
        // order, which is what keeps the horizon level. The other order rolls the view as
        // soon as both are non-zero.
        transform.rotation = Quat::from_rotation_y(look.yaw) * Quat::from_rotation_x(look.pitch);
    }
}
