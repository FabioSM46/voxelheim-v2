//! Draugr and vargr bodies, drawn from the newest authoritative snapshot.
//!
//! A mob exists exactly while the newest snapshot names its id. There is no local AI, no
//! collision, no pathfinding and no inference: **zero health is not read as death**,
//! because a draugr that walked out of the view cube and one that was killed look
//! identical from here, and only one of them is dead. The server stops sending it; this
//! module despawns it; nobody guesses which happened.
//!
//! Everything animated below is cosmetic and follows an authoritative transition rather
//! than deciding one. A windup pose plays because the server said `Windup`, and it ends
//! when the server says something else. **Every kind shares those presentation rules**:
//! a vargr leans and flashes through exactly the code a draugr does, on the timings the
//! server's registry chose, because a second copy of them here would be a second thing to
//! keep in step and would decide nothing either way.
//!
//! ## The bodies are mirrored from the server, and must stay in step with it
//!
//! [`DRAUGR_BODY`], [`VARGR_BODY`] and [`DEER_BODY`] are copies of each row's `body` in
//! `server/internal/game/species.go`, which is where collision, the swing's reach and the
//! spawn separation all read it from. The server collides that box and this side draws
//! inside it, so a mismatch is a creature that visibly does not fill the space a swing
//! reaches — the same rule `PLAYER_WIDTH`/`PLAYER_HEIGHT` already follow in
//! [`super::constants`], and [`the_drawn_body_is_the_box_the_server_collides`] is what
//! holds it.

use std::collections::{HashMap, HashSet};
use std::f32::consts::FRAC_PI_2;
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::camera::WorldCamera;
use super::interpolate::{InterpolatedMob, SnapshotBuffer};
use super::{InputMode, merge_all};
use crate::net::{MobAction, MobKind, Session};

/// The box one species occupies, in blocks: square in plan, `height` tall, standing on
/// the point the snapshot puts it at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Body {
    pub(super) width: f32,
    pub(super) height: f32,
}

/// A corpse on two legs, and a beast on four.
///
/// Mirrored from `mobRegistry` in `server/internal/game/species.go`. The vargr is wider
/// and much shorter than a draugr, and the width is the half that matters most: it is
/// what puts a vargr inside a sword's reach where a draugr is not, because the server
/// measures a swing body to body.
const DRAUGR_BODY: Body = Body {
    width: 0.6,
    height: 1.8,
};
const VARGR_BODY: Body = Body {
    width: 0.9,
    height: 1.0,
};
const DEER_BODY: Body = Body {
    width: 0.9,
    height: 1.4,
};

/// The box a resident occupies: a person's, because a resident is a person.
///
/// Mirrored from `residentBody` in `server/internal/game/resident.go`, which is
/// `{PlayerWidth, PlayerHeight}`. Read through [`super::constants`] rather than written as
/// two more literals, because those are already this client's copy of the server's numbers
/// — so a resident's box cannot drift from a player's without the existing mirror moving.
///
/// It equals [`DRAUGR_BODY`] today, and that is two rows agreeing rather than an alias. It
/// used to *be* one: until #458 this arm returned the draugr's row, which was the honest
/// placeholder while nothing sent a villager.
const VILLAGER_BODY: Body = Body {
    width: crate::player::constants::PLAYER_WIDTH,
    height: crate::player::constants::PLAYER_HEIGHT,
};

// A presentation envelope only. Paddock horses are deliberately absent from the
// server's mob registry and this module routes them to `player/horse.rs`, so no combat,
// marker or corpse path reads this row. Keeping `body` total still forces a new wire kind
// to make its routing choice explicit. The width is the mounted body's, mirrored from the
// server; the height is the drawn horse's own ear tips.
const HORSE_BODY: Body = Body {
    width: crate::player::constants::MOUNTED_WIDTH,
    height: super::horse::HORSE_HEIGHT,
};

/// The body envelope for one kind. It is the box the server collides for creatures and
/// people; Horse is the explicit presentation-only exception above. Total over
/// [`MobKind`], with no wildcard arm, so a new species does not compile until it has been
/// given a body or routed away deliberately.
///
/// **A villager has a row here and is drawn by nobody in this module.** The box is a fact
/// about the world, and it belongs beside the three it has to stay in step with. What
/// *draws* a resident is the humanoid rig in `player/mod.rs`; [`MobVisuals::of`] is where
/// that routing is stated, and it answers `None`.
pub(super) const fn body(kind: MobKind) -> Body {
    match kind {
        MobKind::Draugr => DRAUGR_BODY,
        MobKind::Vargr => VARGR_BODY,
        MobKind::Deer => DEER_BODY,
        MobKind::Villager => VILLAGER_BODY,
        MobKind::Horse => HORSE_BODY,
    }
}

/// How much of a draugr's height the head takes.
const HEAD_EDGE: f32 = 0.34;

/// The draugr keeps the server's 0.6-wide box by narrowing its chest and letting two
/// separately posed arms take the width back. The arms are authored from this shoulder
/// line downwards, with their child origin on that line so a swing is rotation only.
const DRAUGR_CHEST_WIDTH: f32 = 0.34;
const DRAUGR_CHEST_DEPTH: f32 = 0.54;
const DRAUGR_SHOULDER_HEIGHT: f32 = DRAUGR_BODY.height - HEAD_EDGE;
const DRAUGR_ARM_WIDTH: f32 = 0.13;
const DRAUGR_ARM_DEPTH: f32 = 0.13;
const DRAUGR_ARM_LENGTH: f32 = 0.76;
const DRAUGR_ARM_X: f32 = (DRAUGR_BODY.width - DRAUGR_ARM_WIDTH) / 2.0;

/// A raised arm is behind the shoulder in the canonical -Z facing; recovery brings it
/// down and just forward of the body. The strike stays short of the torso's front face,
/// so the hand never swings through the creature it belongs to.
const DRAUGR_ARM_RAISED: f32 = -1.12;
const DRAUGR_ARM_STRIKE: f32 = 0.20;

/// Draugr arms carry the telegraph now. Retaining fifteen percent of the shared lean keeps
/// weight in the blow without leaving the old whole-body headbutt as the attack.
const DRAUGR_LEAN_FRACTION: f32 = 0.15;

/// The vargr, in fractions of its own box: a narrow torso under broken flank and back
/// tufts, four short legs, a heavy shoulder ruff that reaches the top of the box, and a
/// low head thrust out along the facing.
///
/// **Low and long is the whole point of the silhouette.** A draugr is 0.6 wide and 1.8
/// tall and a remote player's capsule is the same; a vargr at 0.9 by 1.0 reads as neither
/// from any distance, which is what "tell a vargr from a draugr before it is close enough
/// to bite" asks for.
const VARGR_TORSO: Vec3 = Vec3::new(0.72, 0.40, 0.52);
const VARGR_TORSO_CENTRE: Vec3 = Vec3::new(0.0, 0.42, 0.14);
const VARGR_RUFF: Vec3 = Vec3::new(0.44, 0.34, 0.34);
const VARGR_HEAD: Vec3 = Vec3::new(0.38, 0.32, 0.28);
const VARGR_HEAD_CENTRE: Vec3 = Vec3::new(0.0, 0.38, -0.28);
const VARGR_MUZZLE: Vec3 = Vec3::new(0.28, 0.16, 0.08);
const VARGR_EYE: Vec3 = Vec3::new(0.065, 0.055, 0.025);
const VARGR_FANG: Vec3 = Vec3::new(0.04, 0.10, 0.03);
const VARGR_LEG: Vec3 = Vec3::new(0.16, 0.24, 0.16);
const VARGR_LEG_SPREAD: Vec3 = Vec3::new(0.26, 0.0, 0.26);

/// A deer's raised body, slim legs, upright neck and broad ears. The torso spans the
/// collision box front-to-back; the ears span it side-to-side, making the complete mesh
/// fill the server-owned 0.9 by 1.4 body without pretending the animal is a cuboid.
const DEER_TORSO: Vec3 = Vec3::new(0.55, 0.50, DEER_BODY.width);
const DEER_TORSO_CENTRE: Vec3 = Vec3::new(0.0, 0.70, 0.0);
const DEER_LEG: Vec3 = Vec3::new(0.12, 0.48, 0.12);
const DEER_LEG_SPREAD: Vec3 = Vec3::new(0.20, 0.0, 0.30);
const DEER_NECK: Vec3 = Vec3::new(0.24, 0.55, 0.24);
const DEER_HEAD: Vec3 = Vec3::new(0.36, 0.30, 0.38);
const DEER_EARS: Vec3 = Vec3::new(DEER_BODY.width, 0.08, 0.12);

/// How long a hit flash lasts. Short enough to read as an impact rather than a state.
const FLASH_TIME: Duration = Duration::from_millis(180);

/// How far a windup leans the body back, and a recovery forward, in radians.
///
/// Bounded on purpose: the pose says *which* action the server chose, and cannot be
/// mistaken for the action itself progressing.
const WINDUP_LEAN: f32 = -0.35;
const RECOVERY_LEAN: f32 = 0.22;

/// How quickly a pose settles towards its target lean, per second.
const LEAN_RESPONSE: f32 = 12.0;

/// How long a body takes to go down, once the server has said the creature is dead.
///
/// **There is no server number this mirrors, and there is no longer one it could.** This
/// side is never told how long a death lasts and does not need to be: the fall is a curve
/// that *finishes*, and the body then lies where it landed for as long as the snapshots
/// keep naming it. What ends a death is the server no longer sending the creature, which
/// despawns the body through the same path a creature that walked out of view takes — so
/// there is no second clock here and nothing to keep in step.
///
/// It used to be argued against `MobDeathDuration`, which was two and a half seconds the
/// *server* spent between the killing blow and the corpse existing, and this number was
/// deliberately shorter than it so that most of the wait was the body already lying still.
/// That constant is gone (#441): a killing blow now produces the lootable corpse on the
/// tick it lands, and the fall is presentation with nothing waiting behind it. So this is
/// the only number in the game that describes how long a body takes to go over, and
/// changing it changes what a death looks like and nothing else — a player who presses F
/// while the draugr is still tipping gets the loot window, because nothing here is
/// consulted about that.
///
/// Seven hundred milliseconds is about what a body of that size takes to go over under
/// this game's gravity, and short enough to read as a fall rather than as a slump.
const FALL_TIME: Duration = Duration::from_millis(700);

/// How far a humanoid ends up tipped, in radians, and which way.
///
/// A quarter turn, so a draugr finishes flat on its back rather than propped against
/// nothing. Positive, which is *backwards*: every mesh here faces -Z, and a positive
/// rotation about +X takes the top of a body towards +Z. That sentence is not what holds
/// it — [`a_draugr_goes_over_backwards`] is, in world space, because a sign argued in prose
/// is a sign nobody notices flipping.
const DRAUGR_FALL_PITCH: f32 = FRAC_PI_2;

/// How far a four-legged body ends up rolled onto its side, in radians.
///
/// Short of a quarter turn on purpose: a beast whose legs went out from under it comes to
/// rest slumped over on one shoulder, where a full ninety degrees is a carcass somebody
/// rolled. About sixty-six degrees.
const VARGR_COLLAPSE_ROLL: f32 = 1.15;

/// How far out a vargr's legs slide as they give way, as a multiple of where they stand.
const VARGR_LEG_SPLAY: f32 = 1.9;

/// Full-screen modes whose UI owns the view instead of the 3D world. The same rule drops obey.
const HIDDEN_INPUT_MODES: [InputMode; 2] = [InputMode::Inventory, InputMode::Menu];

/// A small planar arrowhead above a mob the server says is hunting this player.
const AGGRO_MARKER_WIDTH: f32 = 0.30;
const AGGRO_MARKER_HEIGHT: f32 = 0.34;
const AGGRO_MARKER_GAP: f32 = 0.20;
const AGGRO_MARKER_COLOUR: Color = Color::srgb(0.92, 0.08, 0.06);

/// A warm amber wash for a corpse the server says this recipient may open.
const LOOTABLE_COLOUR: Color = Color::srgb(0.64, 0.52, 0.27);

/// The undead grey a draugr is drawn in.
const DRAUGR_BODY_COLOUR: Color = Color::srgb(0.36, 0.40, 0.38);
const DRAUGR_HEAD_COLOUR: Color = Color::srgb(0.46, 0.48, 0.44);
const DRAUGR_BANDAGE_COLOUR: Color = Color::srgb(0.72, 0.70, 0.61);
const DRAUGR_EYE_COLOUR: Color = Color::srgb(1.0, 0.03, 0.02);

/// The vargr's pelt: warm and dark where the draugr is cold and pale, so the two are told
/// apart by colour as well as by silhouette at the distance the difference matters.
const VARGR_BODY_COLOUR: Color = Color::srgb(0.26, 0.22, 0.20);
const VARGR_HEAD_COLOUR: Color = Color::srgb(0.38, 0.33, 0.29);
const VARGR_FUR_COLOUR: Color = Color::srgb(0.18, 0.14, 0.13);
const VARGR_FANG_COLOUR: Color = Color::srgb(0.88, 0.84, 0.68);
const VARGR_EYE_COLOUR: Color = Color::srgb(1.0, 0.78, 0.05);
const VARGR_EYE_EMISSIVE: LinearRgba = LinearRgba::rgb(7.0, 4.2, 0.15);

/// Warm hide and a lighter face distinguish prey from both hostile species.
const DEER_BODY_COLOUR: Color = Color::srgb(0.48, 0.30, 0.18);
const DEER_HEAD_COLOUR: Color = Color::srgb(0.66, 0.46, 0.28);

/// The red a hit flashes. Shared by every kind: an impact reads the same whatever was hit.
const FLASH_COLOUR: Color = Color::srgb(0.85, 0.20, 0.18);

// The systems below are registered by `PlayerPlugin` rather than by a plugin of their
// own, exactly as the drop renderer's are. That is not a style choice: they have to run
// *inside* the chain that begins with `ingest_snapshots`, because the buffer they sample
// is filled there. A plugin adding them to the `ApplySnapshots` set instead would order
// them against the set and not against the system, which leaves them free to run before
// the snapshot they are meant to draw has arrived — a body that never spawns.

/// The independently drawn meshes and materials one species is drawn from.
///
/// One handle per independently posed part, so a hit flash stays the single material swap
/// it has always been however many primitives a species is built out of. Multiple colours
/// inside a draugr part are vertex colours rather than extra draw children.
#[derive(Debug, Clone)]
struct SpeciesVisuals {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
    /// The legs, for a species that is drawn with a set that moves on its own. `None` for
    /// one whose legs are part of its body, which a draugr's are — nothing poses them
    /// separately, so nothing gains from a child holding them.
    legs: Option<Handle<Mesh>>,
    /// The paired arms, authored around one shoulder-line pivot. Draugr only: the other
    /// species keep their existing silhouettes and poses.
    arms: Option<Handle<Mesh>>,
    /// A separately lit pair of eyes, for the vargr only. Keeping mesh and material in one
    /// option makes it impossible to spawn emissive geometry without its matching handle.
    eyes: Option<EyeVisuals>,
    body_material: Handle<StandardMaterial>,
    head_material: Handle<StandardMaterial>,
}

#[derive(Debug, Clone)]
struct EyeVisuals {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// The shared meshes and materials every body is drawn from, one entry per species.
///
/// Primitives and hand-written colours, no assets: this issue adds no model, texture or
/// animation file, and a body built from a handful of cuboids is enough to tell a draugr
/// from a vargr and both from a remote player's capsule at a glance.
///
/// Each mesh is authored with its origin at the creature's **feet**, which is where the
/// server puts the position it sends, so the parent transform is the whole of where a body
/// stands and the children carry no offset of their own.
#[derive(Resource, Debug)]
pub(super) struct MobVisuals {
    draugr: SpeciesVisuals,
    vargr: SpeciesVisuals,
    deer: SpeciesVisuals,
    /// One flash for every kind: an impact reads the same whatever was hit.
    flash_material: Handle<StandardMaterial>,
    lootable_material: Handle<StandardMaterial>,
    aggro_marker: Handle<Mesh>,
    aggro_marker_material: Handle<StandardMaterial>,
}

impl MobVisuals {
    /// The pair one kind is drawn from, or `None` for a kind this module does not draw.
    ///
    /// Total over [`MobKind`] with no wildcard arm, so a new species does not compile until
    /// it has been given meshes and colours — or, like the villager, until somebody has said
    /// in as many words that it is drawn somewhere else.
    ///
    /// **`None` is the routing, and it is the whole of it.** A resident travels in the same
    /// `MobState` vector as the draugrs, so this module sees one; it is a person, so it is
    /// drawn on the humanoid rig in `player/mod.rs` through `spawn_body` — the same path a
    /// remote player takes — rather than as the two cuboids a draugr is. Answering `None`
    /// keeps every loop below about creatures: no `Mob` is ever spawned for a villager, so
    /// no aggro marker, no lootable tint and no fall pose can reach one, by construction
    /// rather than by a condition in each of them.
    fn of(&self, kind: MobKind) -> Option<&SpeciesVisuals> {
        match kind {
            MobKind::Draugr => Some(&self.draugr),
            MobKind::Vargr => Some(&self.vargr),
            MobKind::Deer => Some(&self.deer),
            MobKind::Villager => None,
            MobKind::Horse => None,
        }
    }
}

/// One live mob of any known kind, keyed by the identity the server gave it.
#[derive(Component, Debug)]
pub(super) struct Mob {
    entity_id: u64,
    kind: MobKind,
    action: MobAction,

    /// Cosmetic time inside the newest server-sent action. Reset only when that action
    /// changes; it poses the arms and can neither advance nor replace the action itself.
    action_elapsed: Duration,

    /// The angle the arms held when the newest authoritative action began, and the angle
    /// the current curve has reached. Keeping both makes an interrupted windup or a
    /// completed recovery continuous without letting either value choose the next action.
    arm_start_angle: f32,
    arm_angle: f32,
    lootable: bool,

    /// The health the last snapshot reported. A *decrease* is what flashes; anything
    /// else — unchanged, or the higher health of a replacement that reused nothing —
    /// does not.
    health: u16,

    /// How long the current flash has been running, if one is.
    flash: Option<Duration>,

    /// The interpolated facing the newest sample gave it.
    ///
    /// Kept here rather than read back out of the `Transform` every frame. The transform
    /// carries the lean as well, so recovering the yaw from it means an euler round trip
    /// through a rotation this module composed — and a value nothing writes is a value
    /// that never changes, which is a draugr that chases you without ever turning to
    /// look at you.
    yaw: f32,

    /// The lean the pose is easing towards, and where it has got to.
    lean: f32,

    /// How long this body has been going down, or `None` while the server is not saying
    /// it is.
    ///
    /// **It exists while the newest snapshot says [`MobAction::Corpse`]** — or
    /// [`MobAction::Dying`], which this server no longer sends and the contract still
    /// carries — and is recomputed from that every time one arrives, which keeps the fall a
    /// consequence of an authoritative transition rather than a state this side entered on
    /// its own. Local time advances it; nothing reads it back as a fact, and in particular
    /// nothing gates looting on it: whether a corpse can be opened is a question the
    /// snapshot answers, in `accessible_loot_corpses`.
    ///
    /// **It is not a clock the body is counted out on.** What ends a death is the server
    /// dropping the creature from its snapshots, which despawns the body through the same
    /// branch a creature that walked out of view takes. So there is no comparison against
    /// [`FALL_TIME`] anywhere except in the pose, and a body whose fall has finished simply
    /// lies still.
    ///
    /// A body first seen already dying starts its fall from the top, because the wire
    /// carries no elapsed time and inventing one would be this side guessing at a moment it
    /// was not told about. The visible cost is a body that streams into view part way
    /// through its death and then falls from upright — rarer than the case it would take to
    /// fix, which is a field on `MobState`.
    ///
    /// A body first seen already a *corpse* is the other half of that and goes the other
    /// way: it lands flat, at [`FALL_TIME`], because a corpse that has been lying there
    /// since before this client could see it should not stand up in order to fall over.
    /// The two are told apart by where they are decided — [`spawn_mob`] has no previous
    /// snapshot and the update path does — and never by a remembered action.
    falling: Option<Duration>,
}

/// Which part of a body one child mesh draws.
///
/// Four where the flash only ever needed two. Legs and arms are parts because each has a
/// transform of its own; the flash still asks one question — head or not — so every new
/// draugr primitive remains covered by the same one material swap per child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MobPart {
    Body,
    Head,
    Legs,
    Arms,
    /// Vargr only. These stay emissive through hit flash and the lootable amber wash: the
    /// rest of the creature carries those states while its night-time face stays readable.
    Eyes,
}

/// Marks the child meshes so a flash can recolour them, and a collapse can splay them,
/// without touching the parent.
#[derive(Component, Debug)]
pub(super) struct MobVisual {
    owner: Entity,
    part: MobPart,
}

/// The billboard child attached only while its owner hunts this session's entity.
#[derive(Component, Debug)]
pub(super) struct AggroMarker {
    owner: Entity,
}

pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Draugr meshes carry their body, bandage and eye colours per vertex. One neutral,
    // lit material lets all three parts share those colours without a texture or another
    // material binding; swapping that handle still flashes and amber-washes every vertex,
    // eyes included.
    let draugr_material = materials.add(StandardMaterial::from_color(Color::WHITE));
    // Vargr coat and fang colours live on vertices so fur, face and teeth stay within the
    // existing body/head draws. Reusing this neutral handle for both leaves the emissive
    // eyes as the creature's only extra material binding.
    let vargr_material = materials.add(StandardMaterial::from_color(Color::WHITE));
    commands.insert_resource(MobVisuals {
        draugr: SpeciesVisuals {
            body: meshes.add(draugr_body_mesh()),
            head: meshes.add(draugr_head_mesh()),
            legs: None,
            arms: Some(meshes.add(draugr_arms_mesh())),
            eyes: None,
            body_material: draugr_material.clone(),
            head_material: draugr_material,
        },
        vargr: SpeciesVisuals {
            body: meshes.add(vargr_body_mesh()),
            head: meshes.add(vargr_head_mesh()),
            legs: Some(meshes.add(vargr_legs_mesh())),
            arms: None,
            eyes: Some(EyeVisuals {
                mesh: meshes.add(vargr_eye_mesh()),
                material: materials.add(StandardMaterial {
                    base_color: VARGR_EYE_COLOUR,
                    emissive: VARGR_EYE_EMISSIVE,
                    perceptual_roughness: 1.0,
                    ..default()
                }),
            }),
            body_material: vargr_material.clone(),
            head_material: vargr_material,
        },
        deer: SpeciesVisuals {
            body: meshes.add(deer_body_mesh()),
            head: meshes.add(deer_head_mesh()),
            legs: Some(meshes.add(deer_legs_mesh())),
            arms: None,
            eyes: None,
            body_material: materials.add(StandardMaterial::from_color(DEER_BODY_COLOUR)),
            head_material: materials.add(StandardMaterial::from_color(DEER_HEAD_COLOUR)),
        },
        flash_material: materials.add(StandardMaterial::from_color(FLASH_COLOUR)),
        lootable_material: materials.add(StandardMaterial::from_color(LOOTABLE_COLOUR)),
        aggro_marker: meshes.add(aggro_marker_mesh()),
        aggro_marker_material: materials.add(StandardMaterial {
            base_color: AGGRO_MARKER_COLOUR,
            unlit: true,
            // A billboard can be viewed from either face while the camera crosses it.
            cull_mode: None,
            ..default()
        }),
    });
}

// Every mesh below reads its extents through [`body`] rather than from the constant
// beside it. That is one indirection for one reason: it makes the mirrored registry the
// single path from the server's number to the geometry, so the sweep that compares the
// two is comparing something rather than restating one constant twice.

/// Gives one primitive the linear vertex colour Bevy's PBR mesh pipeline consumes.
/// Draugr geometry needs three colours inside one child without a texture or material per
/// strip; every primitive is coloured before merging so all merged layouts remain equal.
fn draugr_tint(mesh: Mesh, colour: Color) -> Mesh {
    let linear = colour.to_linear().to_f32_array();
    let vertices = mesh.count_vertices();
    mesh.with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![linear; vertices])
}

fn draugr_box(size: Vec3, centre: Vec3, colour: Color) -> Mesh {
    draugr_tint(
        Mesh::from(Cuboid::from_size(size)).translated_by(centre),
        colour,
    )
}

/// Gives the vargr several coat and face colours without turning each tuft or tooth into a
/// child draw. The shared white material preserves these colours until a flash or loot wash
/// deliberately replaces that one handle for the whole part.
fn vargr_tint(mesh: Mesh, colour: Color) -> Mesh {
    let linear = colour.to_linear().to_f32_array();
    let vertices = mesh.count_vertices();
    mesh.with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![linear; vertices])
}

fn vargr_box(size: Vec3, centre: Vec3, colour: Color) -> Mesh {
    vargr_tint(
        Mesh::from(Cuboid::from_size(size)).translated_by(centre),
        colour,
    )
}

/// The draugr's narrowed torso and its proud horizontal wrappings, standing on the feet.
/// Bands take the depth back to the collision box while the arms take its width back.
fn draugr_body_mesh() -> Mesh {
    let draugr = body(MobKind::Draugr);
    let height = draugr.height - HEAD_EDGE;
    let mut torso = draugr_box(
        Vec3::new(DRAUGR_CHEST_WIDTH, height, DRAUGR_CHEST_DEPTH),
        Vec3::Y * (height / 2.0),
        DRAUGR_BODY_COLOUR,
    );

    let bands = [0.28, 0.48, 0.68, 0.88, 1.08, 1.28].map(|y| {
        draugr_box(
            Vec3::new(DRAUGR_CHEST_WIDTH + 0.035, 0.045, draugr.width),
            Vec3::new(0.0, y, 0.0),
            DRAUGR_BANDAGE_COLOUR,
        )
    });
    // One loose end breaks the otherwise perfect rings and makes the wrapping geometry
    // rather than stripes painted on a box. It stays inside the same exact outer depth.
    let loose_end = draugr_box(
        Vec3::new(0.045, 0.22, 0.025),
        Vec3::new(0.13, 0.93, -draugr.width / 2.0 + 0.0125),
        DRAUGR_BANDAGE_COLOUR,
    );
    merge_all(
        &mut torso,
        bands.into_iter().chain([loose_end]),
        "draugr torso",
    );
    torso
}

/// The bandaged head and two red eyes. The eyes are deliberately ordinary lit geometry,
/// not an exempt emissive child: they remain as visible as the body under the same night
/// lighting, and flash or turn amber with the whole Head through one material swap.
fn draugr_head_mesh() -> Mesh {
    let draugr = body(MobKind::Draugr);
    let mut head = draugr_box(
        Vec3::splat(HEAD_EDGE),
        Vec3::Y * (draugr.height - HEAD_EDGE / 2.0),
        DRAUGR_HEAD_COLOUR,
    );
    let bands = [1.51, 1.61, 1.76].map(|y| {
        draugr_box(
            Vec3::new(HEAD_EDGE + 0.025, 0.035, HEAD_EDGE + 0.025),
            Vec3::new(0.0, y, 0.0),
            DRAUGR_BANDAGE_COLOUR,
        )
    });
    let eyes = [-0.075, 0.075].map(|x| {
        draugr_box(
            Vec3::new(0.055, 0.045, 0.025),
            Vec3::new(x, 1.69, -HEAD_EDGE / 2.0 - 0.0125),
            DRAUGR_EYE_COLOUR,
        )
    });
    let loose_end = draugr_box(
        Vec3::new(0.035, 0.18, 0.025),
        Vec3::new(HEAD_EDGE / 2.0 + 0.0175, 1.58, 0.10),
        DRAUGR_BANDAGE_COLOUR,
    );
    merge_all(
        &mut head,
        bands.into_iter().chain(eyes).chain([loose_end]),
        "draugr head",
    );
    head
}

/// Both arms in one mesh, authored below a shared shoulder-line origin.
///
/// Each hand has a palm and three narrow, separated fingers. The pale rings sit proud of
/// narrower grey limbs, and a loose strip hangs from each forearm. Keeping all of it in
/// this one child makes the attack one transform and one draw part, however many cuboids
/// make the silhouette read.
fn draugr_arms_mesh() -> Mesh {
    let mut parts = Vec::new();
    for side in [-1.0, 1.0] {
        let x = side * DRAUGR_ARM_X;
        parts.push(draugr_box(
            Vec3::new(0.095, DRAUGR_ARM_LENGTH, DRAUGR_ARM_DEPTH - 0.02),
            Vec3::new(x, -DRAUGR_ARM_LENGTH / 2.0, 0.0),
            DRAUGR_BODY_COLOUR,
        ));
        parts.push(draugr_box(
            Vec3::new(DRAUGR_ARM_WIDTH, 0.18, DRAUGR_ARM_DEPTH),
            Vec3::new(x, -DRAUGR_ARM_LENGTH - 0.09, -0.01),
            DRAUGR_BODY_COLOUR,
        ));

        for y in [-0.18, -0.36, -0.55] {
            parts.push(draugr_box(
                Vec3::new(DRAUGR_ARM_WIDTH, 0.04, DRAUGR_ARM_DEPTH),
                Vec3::new(x, y, 0.0),
                DRAUGR_BANDAGE_COLOUR,
            ));
        }

        // Gaps wider than each finger leave daylight between them from front and back;
        // their stagger in Z keeps the hand from collapsing into one end-cap in profile.
        for (finger_x, finger_z) in [(-0.042, -0.028), (0.0, -0.052), (0.042, -0.028)] {
            parts.push(draugr_box(
                Vec3::new(0.022, 0.14, 0.026),
                Vec3::new(x + finger_x, -DRAUGR_ARM_LENGTH - 0.23, finger_z),
                DRAUGR_BODY_COLOUR,
            ));
        }
        parts.push(draugr_box(
            Vec3::new(0.025, 0.17, 0.025),
            Vec3::new(x - side * 0.045, -0.50, -0.07),
            DRAUGR_BANDAGE_COLOUR,
        ));
    }

    let mut parts = parts.into_iter();
    let mut arms = parts.next().expect("a draugr has two arms");
    merge_all(&mut arms, parts, "draugr arms");
    arms
}

/// The vargr's body: a narrow torso under a broken coat and a heavy shoulder ruff.
///
/// Authored in the canonical facing — North is -Z, so the head end is -Z and the haunches
/// are +Z — and merged into one mesh, so a hit flash stays a single material swap.
fn vargr_body_mesh() -> Mesh {
    let vargr = body(MobKind::Vargr);
    let mut torso = vargr_box(VARGR_TORSO, VARGR_TORSO_CENTRE, VARGR_BODY_COLOUR);

    // The ruff overlaps both torso and head instead of perching on the back. Its uneven
    // crown is assembled from a core and three darker steps; the centre step is still the
    // highest thing a vargr has and takes the drawing to the full collided height.
    let ruff_core = vargr_box(VARGR_RUFF, Vec3::new(0.0, 0.78, -0.08), VARGR_FUR_COLOUR);
    let ruff_steps = [
        (Vec3::new(0.15, 0.14, 0.16), Vec3::new(-0.14, 0.90, -0.13)),
        (
            Vec3::new(0.14, 0.20, 0.18),
            Vec3::new(0.01, vargr.height - 0.10, -0.08),
        ),
        (Vec3::new(0.12, 0.12, 0.15), Vec3::new(0.15, 0.89, -0.02)),
    ]
    .map(|(size, centre)| vargr_box(size, centre, VARGR_FUR_COLOUR));

    // The torso itself stops short of the collision walls. Discrete tufts take the flanks
    // and haunches back out to those walls, leaving gaps between their outer faces so the
    // outline is shag rather than one smooth cuboid. Nothing crosses the server-owned box.
    let flank_tufts = [
        (-1.0, -0.08, 0.43, 0.15),
        (-1.0, 0.16, 0.48, 0.18),
        (-1.0, 0.34, 0.41, 0.13),
        (1.0, -0.03, 0.46, 0.18),
        (1.0, 0.20, 0.41, 0.14),
        (1.0, 0.35, 0.50, 0.19),
    ]
    .map(|(side, z, y, height)| {
        vargr_box(
            Vec3::new(0.09, height, 0.13),
            Vec3::new(side * 0.405, y, z),
            VARGR_FUR_COLOUR,
        )
    });
    let haunch_tufts =
        [(-0.22, 0.47, 0.16), (0.0, 0.54, 0.20), (0.22, 0.45, 0.14)].map(|(x, y, height)| {
            vargr_box(
                Vec3::new(0.18, height, 0.10),
                Vec3::new(x, y, vargr.width / 2.0 - 0.05),
                VARGR_FUR_COLOUR,
            )
        });
    let back_tufts = [
        (Vec3::new(-0.25, 0.69, 0.10), 0.14),
        (Vec3::new(-0.07, 0.72, 0.23), 0.20),
        (Vec3::new(0.13, 0.68, 0.31), 0.12),
        (Vec3::new(0.28, 0.70, 0.13), 0.16),
    ]
    .map(|(centre, height)| vargr_box(Vec3::new(0.13, height, 0.14), centre, VARGR_FUR_COLOUR));

    merge_all(
        &mut torso,
        std::iter::once(ruff_core)
            .chain(ruff_steps)
            .chain(flank_tufts)
            .chain(haunch_tufts)
            .chain(back_tufts),
        "vargr coat",
    );
    torso
}

/// The vargr's four legs, in a mesh of their own.
///
/// **They were merged into the body until a death needed to move them**, and they are split
/// out for exactly that: a part a pose moves has to be a child something can hold a
/// transform on, and [`collapse`] slides these outwards as the beast goes down. Nothing
/// else about the split matters — the hit flash recolours everything that is not the head,
/// and these are not the head, so it is still one material swap per body.
///
/// Authored in the same frame the rest of the vargr is, with each leg standing on the
/// origin plane, so the group's transform is a scale about the point the body stands on.
fn vargr_legs_mesh() -> Mesh {
    let mut standing = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
        .map(|(sx, sz): (f32, f32)| {
            vargr_box(
                VARGR_LEG,
                Vec3::new(
                    sx * VARGR_LEG_SPREAD.x,
                    VARGR_LEG.y / 2.0,
                    sz * VARGR_LEG_SPREAD.z,
                ),
                VARGR_BODY_COLOUR,
            )
        })
        .into_iter();

    // Four by construction, so the first is always there; the expect names the invariant
    // rather than hoping for it.
    let mut legs = standing.next().expect("a vargr has four legs");
    merge_all(&mut legs, standing, "vargr legs");
    legs
}

/// The vargr's low head, broad muzzle, ears and bared fangs, all in one flashing part.
fn vargr_head_mesh() -> Mesh {
    let mut head = vargr_box(VARGR_HEAD, VARGR_HEAD_CENTRE, VARGR_HEAD_COLOUR);
    // The muzzle stops short of the collision wall so the eyes and fangs can sit visibly
    // proud of it while those details, rather than another invisible box, reach -0.45.
    let muzzle = vargr_box(VARGR_MUZZLE, Vec3::new(0.0, 0.32, -0.39), VARGR_HEAD_COLOUR);
    let ears = [-1.0, 1.0].map(|side| {
        vargr_box(
            Vec3::new(0.10, 0.18, 0.09),
            Vec3::new(side * 0.13, 0.58, -0.24),
            VARGR_FUR_COLOUR,
        )
    });
    let fangs = [-1.0, 1.0].map(|side| {
        vargr_box(
            VARGR_FANG,
            Vec3::new(side * 0.09, 0.23, -0.435),
            VARGR_FANG_COLOUR,
        )
    });
    merge_all(
        &mut head,
        std::iter::once(muzzle).chain(ears).chain(fangs),
        "vargr head",
    );
    head
}

/// Two emissive eyes, kept in one additional child and against the face's front plane.
fn vargr_eye_mesh() -> Mesh {
    let mut eyes = [-1.0, 1.0]
        .map(|side| {
            Mesh::from(Cuboid::from_size(VARGR_EYE)).translated_by(Vec3::new(
                side * 0.09,
                0.42,
                -0.4325,
            ))
        })
        .into_iter();
    let mut pair = eyes.next().expect("a vargr has two eyes");
    merge_all(&mut pair, eyes, "vargr eyes");
    pair
}

/// The deer's long torso, aligned with the canonical -Z facing.
fn deer_body_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(DEER_TORSO)).translated_by(DEER_TORSO_CENTRE)
}

/// Four slim legs standing on the snapshot position.
fn deer_legs_mesh() -> Mesh {
    let leg = Cuboid::from_size(DEER_LEG);
    let mut standing = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
        .map(|(sx, sz): (f32, f32)| {
            Mesh::from(leg).translated_by(Vec3::new(
                sx * DEER_LEG_SPREAD.x,
                DEER_LEG.y / 2.0,
                sz * DEER_LEG_SPREAD.z,
            ))
        })
        .into_iter();
    let mut legs = standing.next().expect("a deer has four legs");
    merge_all(&mut legs, standing, "deer legs");
    legs
}

/// The neck, face and ears, merged so the hit flash remains one material swap.
fn deer_head_mesh() -> Mesh {
    let mut head = Mesh::from(Cuboid::from_size(DEER_HEAD)).translated_by(Vec3::new(
        0.0,
        DEER_BODY.height - DEER_HEAD.y / 2.0,
        -(DEER_BODY.width - DEER_HEAD.z) / 2.0,
    ));
    let neck = Mesh::from(Cuboid::from_size(DEER_NECK)).translated_by(Vec3::new(0.0, 1.02, -0.16));
    let ears = Mesh::from(Cuboid::from_size(DEER_EARS)).translated_by(Vec3::new(0.0, 1.32, -0.24));
    merge_all(&mut head, [neck, ears], "deer head");
    head
}

/// A downward arrowhead quad on one camera-facing plane.
///
/// This is world geometry rather than `bevy_ui`, so it inherits the body's visibility
/// and cannot remain on screen while the body is hidden. Its bottom edge is deliberately
/// narrow, so the four-vertex quad reads as a triangle without a texture or another asset.
fn aggro_marker_mesh() -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [-AGGRO_MARKER_WIDTH / 2.0, AGGRO_MARKER_HEIGHT / 2.0, 0.0],
            [AGGRO_MARKER_WIDTH / 2.0, AGGRO_MARKER_HEIGHT / 2.0, 0.0],
            [AGGRO_MARKER_WIDTH / 30.0, -AGGRO_MARKER_HEIGHT / 2.0, 0.0],
            [-AGGRO_MARKER_WIDTH / 30.0, -AGGRO_MARKER_HEIGHT / 2.0, 0.0],
        ],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4])
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 0.0], [1.0, 0.0], [0.53, 1.0], [0.47, 1.0]],
    )
    .with_inserted_indices(Indices::U32(vec![0, 3, 2, 0, 2, 1]))
}

/// Spawns, places and despawns bodies from the latest authoritative snapshot.
///
/// The newest snapshot is the existence set, exactly as it is for drops. Nothing here
/// reads health to decide whether a mob still exists.
pub(super) fn apply_snapshots(
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mode: Res<InputMode>,
    visuals: Option<Res<MobVisuals>>,
    mut existing: Query<(Entity, &mut Mob, &mut Transform, &mut Visibility)>,
    markers: Query<(Entity, &AggroMarker)>,
    mut commands: Commands,
) {
    let (Some(session), Some(visuals)) = (session, visuals) else {
        return;
    };

    let interval = Duration::from_secs(1) / u32::from(session.0.tick_rate);
    let drawn = buffer.sample_mobs(Instant::now(), interval);
    let lootable: HashSet<u64> = buffer.accessible_loot_corpses().iter().copied().collect();
    let by_id: HashMap<u64, InterpolatedMob> = drawn.iter().copied().collect();
    let mut placed = HashSet::with_capacity(drawn.len());
    let visibility = mob_visibility(*mode);
    let mut markers_by_owner: HashMap<Entity, Entity> = markers
        .iter()
        .map(|(entity, marker)| (marker.owner, entity))
        .collect();

    for (entity, mut mob, mut transform, mut current_visibility) in &mut existing {
        let Some(state) = by_id.get(&mob.entity_id) else {
            // Gone from the newest answer, so gone from this world. Why is not asked.
            commands.entity(entity).despawn();
            continue;
        };

        transform.translation = state.pos;
        mob.yaw = state.yaw;
        if *current_visibility != visibility {
            *current_visibility = visibility;
        }

        // A *decrease* only. A replacement draugr arrives with a fresh identity, so its
        // full health is a new body rather than a heal — and an unchanged number is not
        // an impact however many frames it survives.
        if state.health < mob.health {
            mob.flash = Some(Duration::ZERO);
        }
        mob.health = state.health;
        if mob.action != state.action {
            mob.action_elapsed = Duration::ZERO;
            mob.arm_start_angle = mob.arm_angle;
        }
        mob.action = state.action;
        mob.kind = state.kind;
        mob.lootable = state.action == MobAction::Corpse && lootable.contains(&mob.entity_id);
        // **The fall exists while the server says dying or corpse**, and it
        // is recomputed from the action every time a snapshot lands rather than started by
        // an edge this side detected. Written this way round so there is no transition to
        // miss: any other action has no fall, whatever the previous one was.
        //
        // **`unwrap_or` is where "was it alive last snapshot?" is answered**, and it is
        // answered without remembering the previous action anywhere. A body that was alive
        // has no fall — every living action clears it, one line above, every snapshot — so
        // `None` here *is* the alive-to-dead edge, and it starts the fall from upright at
        // zero. `Some` is a fall already in progress, which is preserved. The only body
        // that starts already landed is one first *seen* as a corpse, and that case never
        // reaches this arm at all: it is [`spawn_mob`], which has no previous snapshot to
        // have been alive in.
        //
        // Corpse used to start at [`FALL_TIME`] here as well as there, because the server
        // sent `Dying` for two and a half seconds first and a creature only ever became a
        // corpse *after* its fall had run. It no longer does — a killing blow produces the
        // corpse on the tick it lands (#441) — so the snapshot that says Corpse is the one
        // that used to say Dying, and it is the one the fall now starts on.
        mob.falling = match state.action {
            MobAction::Dying | MobAction::Corpse => Some(mob.falling.unwrap_or(Duration::ZERO)),
            _ => None,
        };

        let hunts_local = state.target_entity_id == session.0.entity_id;
        match (markers_by_owner.remove(&entity), hunts_local) {
            (None, true) => spawn_aggro_marker(&mut commands, &visuals, entity, state.kind),
            (Some(marker), false) => commands.entity(marker).despawn(),
            _ => {}
        }

        placed.insert(mob.entity_id);
    }

    for (entity_id, state) in &drawn {
        if !placed.insert(*entity_id) {
            continue;
        }
        spawn_mob(
            &mut commands,
            &visuals,
            *entity_id,
            state,
            lootable.contains(entity_id),
            visibility,
            state.target_entity_id == session.0.entity_id,
        );
    }
}

fn spawn_mob(
    commands: &mut Commands,
    visuals: &MobVisuals,
    entity_id: u64,
    state: &InterpolatedMob,
    lootable: bool,
    visibility: Visibility,
    hunts_local: bool,
) {
    // **The one place a resident is turned away**, by the same question every other kind is
    // asked — see [`MobVisuals::of`]. `player::apply_snapshots` draws it instead, from the
    // same snapshot on the same frame. Nothing below runs, so no `Mob` exists for a villager
    // and nothing hanging off one can reach a resident.
    //
    // The species is chosen once, here, and nothing downstream re-asks: a mob whose kind
    // *changed* is a mob the server replaced, and a replacement arrives with a fresh
    // identity, so there is no path on which a body outlives its own shape.
    let Some(species) = visuals.of(state.kind).cloned() else {
        return;
    };

    let owner = commands
        .spawn((
            Mob {
                entity_id,
                kind: state.kind,
                action: state.action,
                action_elapsed: Duration::ZERO,
                arm_start_angle: draugr_arm_initial_angle(state.action),
                arm_angle: draugr_arm_initial_angle(state.action),
                lootable,
                // The first snapshot of a body is not an impact, whatever its health.
                health: state.health,
                flash: None,
                yaw: state.yaw,
                lean: lean_for(state.kind, state.action),
                // A body first seen already dying falls from upright, and one first seen
                // already a corpse is left lying flat — see the field. This is the *only*
                // place a corpse starts at FALL_TIME: reaching here means there was no
                // previous snapshot, so there was no fall to have watched.
                falling: match state.action {
                    MobAction::Dying => Some(Duration::ZERO),
                    MobAction::Corpse => Some(FALL_TIME),
                    _ => None,
                },
            },
            Transform::from_translation(state.pos).with_rotation(Quat::from_rotation_y(state.yaw)),
            visibility,
        ))
        .id();

    commands.entity(owner).with_children(|parent| {
        // Body, head and legs are authored from the feet. Arms are the deliberate second
        // exception after the independently moving legs: their origin is the shoulder
        // line, so their child carries that one translation and a swing needs no matching
        // compensation.
        parent.spawn((
            MobVisual {
                owner,
                part: MobPart::Body,
            },
            Mesh3d(species.body),
            MeshMaterial3d(species.body_material.clone()),
            Transform::default(),
        ));
        parent.spawn((
            MobVisual {
                owner,
                part: MobPart::Head,
            },
            Mesh3d(species.head),
            MeshMaterial3d(species.head_material),
            Transform::default(),
        ));
        if let Some(legs) = species.legs {
            parent.spawn((
                MobVisual {
                    owner,
                    part: MobPart::Legs,
                },
                Mesh3d(legs),
                MeshMaterial3d(species.body_material.clone()),
                Transform::default(),
            ));
        }
        if let Some(arms) = species.arms {
            parent.spawn((
                MobVisual {
                    owner,
                    part: MobPart::Arms,
                },
                Mesh3d(arms),
                MeshMaterial3d(species.body_material),
                Transform::from_translation(Vec3::Y * DRAUGR_SHOULDER_HEIGHT),
            ));
        }
        if let Some(eyes) = species.eyes {
            parent.spawn((
                MobVisual {
                    owner,
                    part: MobPart::Eyes,
                },
                Mesh3d(eyes.mesh),
                MeshMaterial3d(eyes.material),
                Transform::default(),
            ));
        }
        if hunts_local {
            parent.spawn(aggro_marker_bundle(visuals, owner, state.kind));
        }
    });
}

fn aggro_marker_bundle(visuals: &MobVisuals, owner: Entity, kind: MobKind) -> impl Bundle + use<> {
    (
        AggroMarker { owner },
        Mesh3d(visuals.aggro_marker.clone()),
        MeshMaterial3d(visuals.aggro_marker_material.clone()),
        Transform::from_translation(Vec3::Y * (body(kind).height + AGGRO_MARKER_GAP)),
        Visibility::Inherited,
    )
}

fn spawn_aggro_marker(commands: &mut Commands, visuals: &MobVisuals, owner: Entity, kind: MobKind) {
    commands
        .entity(owner)
        .with_child(aggro_marker_bundle(visuals, owner, kind));
}

type MobTransforms<'w, 's> =
    Query<'w, 's, &'static Transform, (With<Mob>, Without<AggroMarker>, Without<WorldCamera>)>;
type AggroMarkerTransforms<'w, 's> = Query<
    'w,
    's,
    (&'static AggroMarker, &'static mut Transform),
    (Without<Mob>, Without<WorldCamera>),
>;

/// Rotates every marker into the camera plane without moving it off its owner's head.
///
/// The desired world rotation is converted back through the mob root because the marker
/// is a child and its owner may be turning, leaning or falling. This is presentation only:
/// neither target ids nor camera positions leave the renderer.
pub(super) fn face_aggro_markers(
    cameras: Query<&Transform, (With<WorldCamera>, Without<AggroMarker>)>,
    owners: MobTransforms<'_, '_>,
    mut markers: AggroMarkerTransforms<'_, '_>,
) {
    let Ok(camera) = cameras.single() else {
        return;
    };

    for (marker, mut transform) in &mut markers {
        let Ok(owner) = owners.get(marker.owner) else {
            continue;
        };
        let world_position = owner.transform_point(transform.translation);
        if camera.translation.distance_squared(world_position) <= f32::EPSILON {
            continue;
        }
        let world_rotation = Transform::from_translation(world_position)
            .looking_at(camera.translation, Vec3::Y)
            .rotation;
        transform.rotation = owner.rotation.inverse() * world_rotation;
    }
}

pub(super) fn mob_visibility(mode: InputMode) -> Visibility {
    if HIDDEN_INPUT_MODES.contains(&mode) {
        Visibility::Hidden
    } else {
        Visibility::Visible
    }
}

/// The lean an action poses at. Bounded, and a function of the action alone.
///
/// A body going down leans at nothing: the whole of its pose is [`collapse`], and easing a
/// windup's lean out of the way while the fall eases in is what keeps a creature killed
/// mid-telegraph from finishing that telegraph on its back.
fn lean_for(kind: MobKind, action: MobAction) -> f32 {
    let lean = match action {
        MobAction::Windup => WINDUP_LEAN,
        MobAction::Recovery => RECOVERY_LEAN,
        MobAction::Idle
        | MobAction::Chase
        | MobAction::Flee
        | MobAction::Dying
        | MobAction::Corpse => 0.0,
    };
    match kind {
        MobKind::Draugr => lean * DRAUGR_LEAN_FRACTION,
        MobKind::Vargr | MobKind::Deer | MobKind::Villager | MobKind::Horse => lean,
    }
}

/// The fraction of a cosmetic pose reached after `elapsed`.
///
/// This is the same frame-rate-independent response as the body lean, expressed from
/// elapsed time so [`draugr_arm_swing`] is pure and tests can inspect the whole curve.
/// It is not a copy of any species timing: the server decides when the action changes,
/// and this answer can only pose the action currently on the mob.
fn pose_progress(elapsed: Duration) -> f32 {
    1.0 - (-LEAN_RESPONSE * elapsed.as_secs_f32()).exp()
}

/// The angle a body first seen in one action honestly starts at.
///
/// Recovery follows a windup that already landed, so a body first streamed into view in
/// recovery starts raised. Every other first sight starts neutral; the wire carries no
/// earlier pose to recover.
fn draugr_arm_initial_angle(action: MobAction) -> f32 {
    match action {
        MobAction::Recovery => DRAUGR_ARM_RAISED,
        MobAction::Idle
        | MobAction::Chase
        | MobAction::Flee
        | MobAction::Windup
        | MobAction::Dying
        | MobAction::Corpse => 0.0,
    }
}

/// Rotation about the shoulder line for one server-sent action.
///
/// Windup raises the arms behind the shoulders and recovery moves them down and forward.
/// The curve begins at the angle the previous authoritative action actually reached, so
/// an abandoned windup and Recovery -> Chase settle without a discontinuity. Death is
/// still drawn with identity below, so an arm pose never fights the parent's fall.
fn draugr_arm_angle(action: MobAction, start_angle: f32, elapsed: Duration) -> f32 {
    let progress = pose_progress(elapsed);
    let target = match action {
        MobAction::Windup => DRAUGR_ARM_RAISED,
        MobAction::Recovery => DRAUGR_ARM_STRIKE,
        MobAction::Idle
        | MobAction::Chase
        | MobAction::Flee
        | MobAction::Dying
        | MobAction::Corpse => 0.0,
    };
    start_angle + (target - start_angle) * progress
}

fn draugr_arm_swing(action: MobAction, start_angle: f32, elapsed: Duration) -> Quat {
    Quat::from_rotation_x(draugr_arm_angle(action, start_angle, elapsed))
}

/// How far through its fall a body is, from how long it has been going down.
///
/// Squared, so the topple accelerates: something that starts down as fast as it finishes
/// reads as having been pushed rather than as having lost its footing. Clamped at one,
/// which is what makes this an animation that *ends* rather than one that keeps going as
/// long as the body does — see [`Mob::falling`].
fn fallen(elapsed: Duration) -> f32 {
    let progress = (elapsed.as_secs_f32() / FALL_TIME.as_secs_f32()).clamp(0.0, 1.0);
    progress * progress
}

/// The rotation a body has reached on its way down.
///
/// **A humanoid topples and a beast slumps**, which is the one thing about a death that
/// differs by species and the only place in this module a `match` on the kind decides a
/// pose. Both pivot at the feet, because every mesh here is authored with its origin
/// there — so a draugr goes over backwards about its heels and a vargr rolls sideways off
/// its legs, and neither needs anything translated.
///
/// Total over [`MobKind`] with no wildcard arm, for the reason [`body`] is: a new species
/// does not compile until somebody has decided how it falls over.
///
/// The identity at `fallen == 0` is what lets this be composed unconditionally, whether the
/// creature is dying or not.
fn collapse(kind: MobKind, fallen: f32) -> Quat {
    match kind {
        // The villager arm keeps the match total and is unreachable twice over: nothing
        // here spawns a `Mob` for a resident (see [`MobVisuals::of`]), and a resident has
        // no action but `Idle` anyway. Grouped with the draugr because a person that did
        // go over would go over like the other upright figure.
        MobKind::Draugr | MobKind::Villager | MobKind::Horse => {
            Quat::from_rotation_x(DRAUGR_FALL_PITCH * fallen)
        }
        MobKind::Vargr => Quat::from_rotation_z(VARGR_COLLAPSE_ROLL * fallen),
        MobKind::Deer => Quat::from_rotation_z(VARGR_COLLAPSE_ROLL * fallen),
    }
}

/// How far out a species' legs have slid, as a scale on the group they are drawn in.
///
/// Only the vargr has a group to scale, and only the plan axes move: the legs go outwards
/// from under the body while its own height is left alone. **One transform on the group
/// rather than four legs each turning on its own hip**, which is the exchange this whole
/// file makes — a rig is a different kind of thing from a handful of cuboids and a lean.
/// The cost is that the legs thicken by the same factor they travel; at the distance a
/// fight happens at, what reads is the splay.
fn leg_splay(kind: MobKind, fallen: f32) -> Vec3 {
    match kind {
        MobKind::Draugr | MobKind::Villager | MobKind::Horse => Vec3::ONE,
        MobKind::Vargr => {
            let out = 1.0 + (VARGR_LEG_SPLAY - 1.0) * fallen;
            Vec3::new(out, 1.0, out)
        }
        MobKind::Deer => {
            let out = 1.0 + (VARGR_LEG_SPLAY - 1.0) * fallen;
            Vec3::new(out, 1.0, out)
        }
    }
}

/// Runs the cosmetic half: the pose easing towards its action's lean, the fall a killed
/// body is part way through, and the hit flash.
///
/// Local time drives all three, and none of them can change an action, a health or whether
/// a body exists. A flash that outlives its mob simply goes with it, and so does a fall.
///
/// **Ordered inside the `ApplySnapshots` chain, after [`apply_snapshots`]**, and declared
/// there rather than relied on: the fall this advances is started by the action that system
/// writes, so a frame that ran the two the other way round would begin every death one
/// frame late.
pub(super) fn animate(
    time: Res<Time>,
    visuals: Option<Res<MobVisuals>>,
    mut mobs: Query<(Entity, &mut Mob, &mut Transform)>,
    // `Without<Mob>` so Bevy can prove the two `&mut Transform` sets are disjoint: the
    // parents carry `Mob` and the children carry `MobVisual`, and without the filter it
    // refuses the system rather than risk aliasing them.
    mut parts: Query<
        (
            &MobVisual,
            &mut MeshMaterial3d<StandardMaterial>,
            &mut Transform,
        ),
        Without<Mob>,
    >,
) {
    let Some(visuals) = visuals else {
        return;
    };
    let delta = time.delta();

    // Everything a child needs: species and fall pose select its ordinary transforms,
    // the server's action plus local elapsed time poses the draugr's arms, and lootable
    // chooses the authoritative presentation wash.
    let mut poses: HashMap<Entity, (MobKind, f32, bool, Quat)> = HashMap::new();
    let mut flashing = HashSet::new();
    for (entity, mut mob, mut transform) in &mut mobs {
        // Exponential easing towards the target, so the pose is frame-rate independent
        // and never overshoots into a lean the server did not ask for. The timings the
        // pose reads are the server's — `mob.action` is a snapshot field — so a vargr
        // leans and recovers on its own species' clock without a second copy of it here.
        mob.action_elapsed += delta;
        mob.arm_angle = draugr_arm_angle(mob.action, mob.arm_start_angle, mob.action_elapsed);
        let target = lean_for(mob.kind, mob.action);
        let response = 1.0 - (-LEAN_RESPONSE * delta.as_secs_f32()).exp();
        mob.lean += (target - mob.lean) * response;

        let down = match mob.falling.as_mut() {
            Some(elapsed) => {
                *elapsed += delta;
                fallen(*elapsed)
            }
            None => 0.0,
        };

        // The fall goes between the facing and the lean, so it turns the whole body about
        // the feet: the yaw is still the way the creature was pointing when it went, and
        // the lean is whatever is left of a telegraph easing out underneath it.
        transform.rotation = Quat::from_rotation_y(mob.yaw)
            * collapse(mob.kind, down)
            * Quat::from_rotation_x(mob.lean);

        if let Some(elapsed) = mob.flash.as_mut() {
            *elapsed += delta;
            if *elapsed >= FLASH_TIME {
                mob.flash = None;
            } else {
                flashing.insert(entity);
            }
        }

        let arm_swing = if down == 0.0 {
            draugr_arm_swing(mob.action, mob.arm_start_angle, mob.action_elapsed)
        } else {
            Quat::IDENTITY
        };
        poses.insert(entity, (mob.kind, down, mob.lootable, arm_swing));
    }

    for (part, mut material, mut transform) in &mut parts {
        let Some((kind, down, lootable, arm_swing)) = poses.get(&part.owner).copied() else {
            // The body this part hangs under was despawned this frame and the child goes
            // with it. There is nothing left to recolour or to move.
            continue;
        };
        // Unreachable for a kind this module does not draw: no `Mob` is ever spawned for
        // one, so no part hangs under one either.
        let Some(species) = visuals.of(kind) else {
            continue;
        };

        // Vargr eyes are deliberately the exception to both presentation washes. Their
        // emissive material is what makes the face visible in darkness; the other three
        // parts still flash or turn amber, so preserving the eyes hides neither state.
        let next = if part.part == MobPart::Eyes {
            species
                .eyes
                .as_ref()
                .expect("only a species with eye visuals spawns an eye part")
                .material
                .clone()
        } else if flashing.contains(&part.owner) {
            visuals.flash_material.clone()
        } else if lootable {
            visuals.lootable_material.clone()
        } else if part.part == MobPart::Head {
            species.head_material.clone()
        } else {
            species.body_material.clone()
        };
        if material.0 != next {
            material.0 = next;
        }

        // Only the legs ever move relative to the body they hang under, and only while it
        // is going down. Written unconditionally rather than behind a `Dying` check,
        // because `leg_splay` is the identity at rest and a branch here would be a second
        // place deciding when a collapse is happening.
        if part.part == MobPart::Legs {
            let splay = leg_splay(kind, down);
            if transform.scale != splay {
                transform.scale = splay;
            }
        }
        if part.part == MobPart::Arms {
            transform.rotation = arm_swing;
        }
    }
}

#[cfg(test)]
mod tests {
    //! No window, no display and no GPU. Every assertion is about what the *server* said,
    //! or about a cosmetic value local time produced from it.

    use bevy::asset::AssetPlugin;
    use bevy::mesh::VertexAttributeValues;
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{MobState, SessionParams, Snapshot, SnapshotInbox};
    use crate::player::PlayerPlugin;

    const INTERVAL: Duration = Duration::from_millis(50);

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
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn draugr(entity_id: u64, x: f32, health: u16, action: MobAction) -> MobState {
        MobState {
            entity_id,
            kind: MobKind::Draugr,
            pos: [x, 64.0, 0.0],
            vel: [0.0; 3],
            yaw: 0.0,
            health,
            max_health: 60,
            action,
            target_entity_id: 0,
        }
    }

    /// The same body with the other kind on it, at the maximum its own row registers.
    fn vargr(entity_id: u64, x: f32, health: u16, action: MobAction) -> MobState {
        MobState {
            kind: MobKind::Vargr,
            max_health: 35,
            ..draugr(entity_id, x, health, action)
        }
    }

    fn deer(entity_id: u64, x: f32, health: u16, action: MobAction) -> MobState {
        MobState {
            kind: MobKind::Deer,
            max_health: 20,
            ..draugr(entity_id, x, health, action)
        }
    }

    /// A resident exactly as the server sends one. `Idle`, full health and no target are
    /// constants in `resident.state()` (`server/internal/game/resident.go`), not choices.
    fn villager(entity_id: u64, x: f32) -> MobState {
        MobState {
            kind: MobKind::Villager,
            max_health: 100,
            ..draugr(entity_id, x, 100, MobAction::Idle)
        }
    }

    fn headless() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .add_plugins(PlayerPlugin);
        app
    }

    fn deliver(app: &mut App, tick: u32, mobs: Vec<MobState>) {
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: tick,
                mobs,
                ..Default::default()
            },
            Instant::now(),
        );
    }

    fn deliver_lootable(app: &mut App, tick: u32, mob: MobState, accessible: bool) {
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: tick,
                mobs: vec![mob],
                accessible_loot_corpses: accessible.then_some(mob.entity_id).into_iter().collect(),
                ..Default::default()
            },
            Instant::now(),
        );
    }

    fn bodies(app: &mut App) -> Vec<(u64, u16, MobAction)> {
        let world = app.world_mut();
        let mut query = world.query::<&Mob>();
        let mut found: Vec<_> = query
            .iter(world)
            .map(|mob| (mob.entity_id, mob.health, mob.action))
            .collect();
        found.sort_by_key(|(entity_id, _, _)| *entity_id);
        found
    }

    fn flashing(app: &mut App) -> usize {
        let world = app.world_mut();
        let mut query = world.query::<&Mob>();
        query.iter(world).filter(|mob| mob.flash.is_some()).count()
    }

    fn targeting(mut mob: MobState, entity_id: u64) -> MobState {
        mob.target_entity_id = entity_id;
        mob
    }

    fn marker_owners(app: &mut App) -> Vec<(u64, Visibility)> {
        let world = app.world_mut();
        let mut markers = world.query::<(&AggroMarker, &Visibility, &Mesh3d)>();
        let mut mobs = world.query::<&Mob>();
        let mut found: Vec<_> = markers
            .iter(world)
            .map(|(marker, visibility, _)| {
                let owner = mobs
                    .get(world, marker.owner)
                    .expect("marker owner is a mob");
                (owner.entity_id, *visibility)
            })
            .collect();
        found.sort_by_key(|(entity_id, _)| *entity_id);
        found
    }

    /// Every kind the newest snapshot put a body on screen for.
    fn kinds(app: &mut App) -> Vec<(u64, MobKind)> {
        let world = app.world_mut();
        let mut query = world.query::<&Mob>();
        let mut found: Vec<_> = query
            .iter(world)
            .map(|mob| (mob.entity_id, mob.kind))
            .collect();
        found.sort_by_key(|(entity_id, _)| *entity_id);
        found
    }

    /// The meshes and materials every drawn part is built from.
    fn parts(app: &mut App) -> Vec<(Handle<Mesh>, Handle<StandardMaterial>)> {
        let world = app.world_mut();
        let mut query =
            world.query_filtered::<(&Mesh3d, &MeshMaterial3d<StandardMaterial>), With<MobVisual>>();
        query
            .iter(world)
            .map(|(mesh, material)| (mesh.0.clone(), material.0.clone()))
            .collect()
    }

    /// The box every vertex of one species' meshes fits inside, as `(min, max)`.
    fn drawn_extent(meshes: &[Mesh]) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for mesh in meshes {
            let Some(positions) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else {
                panic!("every part carries positions");
            };
            for vertex in positions.as_float3().expect("three floats per position") {
                let vertex = Vec3::from_array(*vertex);
                min = min.min(vertex);
                max = max.max(vertex);
            }
        }
        (min, max)
    }

    #[test]
    fn a_snapshot_with_two_draugr_spawns_two_bodies() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![
                draugr(900, 3.0, 60, MobAction::Idle),
                draugr(901, 9.0, 60, MobAction::Chase),
            ],
        );
        app.update();

        assert_eq!(
            bodies(&mut app),
            vec![(900, 60, MobAction::Idle), (901, 60, MobAction::Chase)]
        );
    }

    #[test]
    fn the_newest_target_moves_the_local_aggro_marker_in_one_frame() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![
                targeting(draugr(900, 3.0, 60, MobAction::Chase), 7),
                targeting(draugr(901, 6.0, 60, MobAction::Chase), 11),
            ],
        );
        app.update();
        assert_eq!(marker_owners(&mut app), vec![(900, Visibility::Inherited)]);

        deliver(
            &mut app,
            2,
            vec![
                draugr(900, 3.0, 60, MobAction::Chase),
                targeting(draugr(901, 6.0, 60, MobAction::Chase), 7),
            ],
        );
        app.update();
        assert_eq!(marker_owners(&mut app), vec![(901, Visibility::Inherited)]);

        deliver(
            &mut app,
            3,
            vec![
                draugr(900, 3.0, 60, MobAction::Chase),
                draugr(901, 6.0, 60, MobAction::Chase),
            ],
        );
        app.update();
        assert!(
            marker_owners(&mut app).is_empty(),
            "a targetless snapshot left a marker"
        );
    }

    #[test]
    fn the_aggro_marker_faces_the_world_camera() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![targeting(draugr(900, 3.0, 60, MobAction::Chase), 7)],
        );
        app.update();

        let world = app.world_mut();
        let mut cameras = world.query_filtered::<&Transform, With<WorldCamera>>();
        let camera = *cameras.single(world).expect("one world camera");
        let mut mobs = world.query_filtered::<&Transform, With<Mob>>();
        let owner = *mobs.single(world).expect("one mob");
        let mut markers = world.query::<(&AggroMarker, &Transform)>();
        let (_, marker) = markers.single(world).expect("one marker");
        let world_position = owner.transform_point(marker.translation);
        let towards_camera = (camera.translation - world_position).normalize();
        let drawn_forward = owner.rotation * marker.rotation * -Vec3::Z;
        assert!(
            drawn_forward.dot(towards_camera) > 0.999,
            "marker forward {drawn_forward:?}, camera direction {towards_camera:?}"
        );
    }

    #[test]
    fn a_fleeing_deer_is_drawn_from_the_authoritative_snapshot() {
        let mut app = headless();
        deliver(&mut app, 1, vec![deer(903, 4.0, 20, MobAction::Flee)]);
        app.update();

        assert_eq!(kinds(&mut app), vec![(903, MobKind::Deer)]);
        assert_eq!(bodies(&mut app), vec![(903, 20, MobAction::Flee)]);
        assert_eq!(lean_for(MobKind::Deer, MobAction::Flee), 0.0);
    }

    /// The newest snapshot is the complete existence set, exactly as it is for drops.
    #[test]
    fn a_draugr_omitted_by_the_newest_snapshot_is_gone() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();
        assert_eq!(bodies(&mut app).len(), 1);

        deliver(&mut app, 2, vec![]);
        app.update();
        assert!(bodies(&mut app).is_empty());
    }

    /// **Zero health is not death.** The server decides whether a draugr still exists by
    /// including it, and a client that despawned on a health of zero would be inventing a
    /// transition — one it would get wrong for a mob that is merely out of the view cube.
    #[test]
    fn a_draugr_at_zero_health_is_not_despawned_by_this_client() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();

        // Zero health, and the server still sending it. It is still there.
        deliver(&mut app, 2, vec![draugr(900, 3.0, 0, MobAction::Idle)]);
        app.update();
        assert_eq!(bodies(&mut app), vec![(900, 0, MobAction::Idle)]);

        // Repeated, so a client that filtered zero-health mobs out of the set it draws
        // has somewhere to act rather than only a first frame to survive.
        for tick in 3..7 {
            deliver(&mut app, tick, vec![draugr(900, 3.0, 0, MobAction::Idle)]);
            app.update();
            assert_eq!(
                bodies(&mut app),
                vec![(900, 0, MobAction::Idle)],
                "a draugr at zero health was despawned by this client on tick {tick}"
            );
        }

        // And a body first *seen* at zero health is drawn too: a client arriving mid-fight
        // is told what exists, not what is healthy.
        deliver(
            &mut app,
            7,
            vec![
                draugr(900, 3.0, 0, MobAction::Idle),
                draugr(901, 5.0, 0, MobAction::Idle),
            ],
        );
        app.update();
        assert_eq!(
            bodies(&mut app),
            vec![(900, 0, MobAction::Idle), (901, 0, MobAction::Idle)]
        );
    }

    #[test]
    fn only_an_authoritative_health_decrease_flashes() {
        let mut app = headless();

        // A short step by default. **The strategy matters more than it looks**: `animate`
        // runs in the same chain that set the flash, so a manual duration of FLASH_TIME
        // would advance every flash past its own window on the frame it began — and every
        // assertion below would read zero whatever the code did.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));

        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();
        assert_eq!(
            flashing(&mut app),
            0,
            "the first sight of a body is not a hit"
        );

        deliver(&mut app, 2, vec![draugr(900, 3.0, 60, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 0, "an unchanged health flashed");

        deliver(&mut app, 3, vec![draugr(900, 3.0, 35, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 1, "a health decrease did not flash");
        let flash = app.world().resource::<MobVisuals>().flash_material.clone();
        let flashed = parts(&mut app);
        assert_eq!(flashed.len(), 3, "the draugr lost a visual part");
        assert!(
            flashed.iter().all(|(_, material)| *material == flash),
            "the hit flash did not recolour body, head/eyes and arms"
        );

        // It ends on local time, without another snapshot.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(FLASH_TIME));
        app.update();
        assert_eq!(flashing(&mut app), 0, "the flash outlived its window");
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));

        // The same body back at *higher* health — the case that separates "a decrease"
        // from "a change". Nothing about a number going up is an impact.
        deliver(&mut app, 4, vec![draugr(900, 3.0, 60, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 0, "a health increase flashed");
    }

    /// A draugr turns to face what it is hunting, and the *transform* is where that has
    /// to show.
    ///
    /// `sample_mobs` computed an interpolated yaw from the first commit; nothing applied
    /// it, so a body kept the facing it spawned with for its whole life. Every test in
    /// this module asserted the sampler's output or the component's fields, and none of
    /// them looked at what was actually drawn — which is exactly where it went wrong.
    #[test]
    fn a_draugr_turns_to_face_where_the_server_says() {
        fn drawn_yaw(app: &mut App) -> f32 {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Transform, With<Mob>>();
            query
                .single(world)
                .expect("one body")
                .rotation
                .to_euler(EulerRot::YXZ)
                .0
        }

        let mut app = headless();
        let mut facing = draugr(900, 3.0, 60, MobAction::Idle);
        facing.yaw = 0.0;
        deliver(&mut app, 1, vec![facing]);
        app.update();
        assert!(drawn_yaw(&mut app).abs() < 1e-4);

        // Two snapshots at the same facing, so the sample is a settled 1.5 rather than a
        // point part way along a segment.
        facing.yaw = 1.5;
        deliver(&mut app, 2, vec![facing]);
        app.update();
        deliver(&mut app, 3, vec![facing]);
        app.update();

        let drawn = drawn_yaw(&mut app);
        assert!(
            (drawn - 1.5).abs() < 1e-3,
            "the body is facing {drawn}, want the 1.5 the server sent"
        );
    }

    /// A replacement draugr arrives with a fresh identity and full health. That is a new
    /// body rather than a heal, and it must not read as one.
    #[test]
    fn a_replacement_draugr_is_a_new_body_rather_than_a_heal() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 10, MobAction::Idle)]);
        app.update();

        deliver(&mut app, 2, vec![draugr(901, 3.0, 60, MobAction::Idle)]);
        app.update();

        assert_eq!(bodies(&mut app), vec![(901, 60, MobAction::Idle)]);
        assert_eq!(flashing(&mut app), 0, "a fresh body flashed");
    }

    /// The pose follows the server's action and cannot advance it.
    #[test]
    fn the_pose_leans_where_the_action_says_and_nowhere_else() {
        assert_eq!(lean_for(MobKind::Draugr, MobAction::Idle), 0.0);
        assert_eq!(lean_for(MobKind::Draugr, MobAction::Chase), 0.0);
        assert_eq!(
            lean_for(MobKind::Draugr, MobAction::Windup),
            WINDUP_LEAN * DRAUGR_LEAN_FRACTION
        );
        assert_eq!(
            lean_for(MobKind::Draugr, MobAction::Recovery),
            RECOVERY_LEAN * DRAUGR_LEAN_FRACTION
        );
        assert_eq!(
            lean_for(MobKind::Vargr, MobAction::Windup),
            WINDUP_LEAN,
            "moving the draugr telegraph changed another species' pose"
        );
        assert_eq!(
            lean_for(MobKind::Deer, MobAction::Recovery),
            RECOVERY_LEAN,
            "moving the draugr telegraph changed another species' pose"
        );

        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Windup)]);
        app.update();

        // The action is the server's word, held until another snapshot changes it.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(INTERVAL));
        for _ in 0..10 {
            app.update();
            assert_eq!(bodies(&mut app), vec![(900, 60, MobAction::Windup)]);
        }
    }

    fn canonical_arm_angle(action: MobAction, elapsed: Duration) -> f32 {
        draugr_arm_angle(action, draugr_arm_initial_angle(action), elapsed)
    }

    /// The arms present the newest authoritative action, and their local clock can only
    /// ease inside that pose. It never promotes Windup to Recovery on its own.
    #[test]
    fn draugr_arms_raise_then_strike_without_overshooting() {
        for action in [
            MobAction::Idle,
            MobAction::Chase,
            MobAction::Flee,
            MobAction::Dying,
            MobAction::Corpse,
        ] {
            assert_eq!(
                draugr_arm_swing(action, 0.0, Duration::from_secs(10)),
                Quat::IDENTITY
            );
        }

        let samples = [0, 25, 50, 100, 250, 1_000]
            .map(|millis| canonical_arm_angle(MobAction::Windup, Duration::from_millis(millis)));
        assert!(samples[0].abs() < 1e-6);
        assert!(
            samples.windows(2).all(|pair| pair[1] <= pair[0]),
            "windup was not monotonic: {samples:?}"
        );
        assert!(
            samples.iter().all(|angle| *angle >= DRAUGR_ARM_RAISED),
            "windup overshot its authored raised pose: {samples:?}"
        );

        let samples = [0, 25, 50, 100, 250, 1_000]
            .map(|millis| canonical_arm_angle(MobAction::Recovery, Duration::from_millis(millis)));
        assert!((samples[0] - DRAUGR_ARM_RAISED).abs() < 1e-6);
        assert!(
            samples.windows(2).all(|pair| pair[1] >= pair[0]),
            "recovery was not monotonic: {samples:?}"
        );
        assert!(
            samples.iter().all(|angle| *angle <= DRAUGR_ARM_STRIKE),
            "recovery overshot through the body: {samples:?}"
        );
        assert!((samples[5] - DRAUGR_ARM_STRIKE).abs() < 1e-4);
    }

    #[test]
    fn a_new_authoritative_action_restarts_only_the_cosmetic_arm_clock() {
        let mut app = headless();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Windup)]);
        app.update();
        for _ in 0..50 {
            app.update();
        }
        let before = {
            let world = app.world_mut();
            let mut query = world.query::<&Mob>();
            query.single(world).expect("one draugr").arm_angle
        };

        deliver(&mut app, 2, vec![draugr(900, 3.0, 60, MobAction::Recovery)]);
        app.update();
        let world = app.world_mut();
        let mut query = world.query::<&Mob>();
        let mob = query.single(world).expect("one draugr");
        assert_eq!(mob.action, MobAction::Recovery);
        assert!(
            mob.action_elapsed <= Duration::from_millis(1),
            "the recovery inherited {:?} of its windup",
            mob.action_elapsed
        );
        assert_eq!(
            mob.arm_start_angle, before,
            "the recovery snapped away from the angle its windup actually reached"
        );
        assert!(
            (mob.arm_angle - before).abs() < 0.02,
            "the first recovery frame jumped from {before} to {}",
            mob.arm_angle
        );

        let partial_recovery = draugr_arm_angle(
            MobAction::Recovery,
            mob.arm_start_angle,
            Duration::from_millis(80),
        );
        assert_eq!(
            draugr_arm_angle(MobAction::Chase, partial_recovery, Duration::ZERO),
            partial_recovery,
            "Recovery -> Chase snapped to neutral at the action boundary"
        );
    }

    #[test]
    fn draugr_arms_rotate_about_the_shared_shoulder_line() {
        let shoulder = Vec3::Y * DRAUGR_SHOULDER_HEIGHT;
        for action in [MobAction::Idle, MobAction::Windup, MobAction::Recovery] {
            for millis in [0, 40, 200, 1_000] {
                let start = draugr_arm_initial_angle(action);
                let child = Transform::from_translation(shoulder).with_rotation(draugr_arm_swing(
                    action,
                    start,
                    Duration::from_millis(millis),
                ));
                assert_eq!(
                    child.transform_point(Vec3::ZERO),
                    shoulder,
                    "{action:?} moved the arm pivot away from the shoulder"
                );
            }
        }
    }

    #[test]
    fn draugr_details_are_geometry_in_one_shared_material() {
        fn colours(mesh: &Mesh) -> &[[f32; 4]] {
            let Some(VertexAttributeValues::Float32x4(colours)) =
                mesh.attribute(Mesh::ATTRIBUTE_COLOR)
            else {
                panic!("draugr geometry has no vertex colours");
            };
            colours
        }

        fn has(mesh: &Mesh, colour: Color) -> bool {
            let expected = colour.to_linear().to_f32_array();
            colours(mesh).contains(&expected)
        }

        let body = draugr_body_mesh();
        let head = draugr_head_mesh();
        let arms = draugr_arms_mesh();
        assert!(has(&body, DRAUGR_BODY_COLOUR));
        assert!(has(&body, DRAUGR_BANDAGE_COLOUR));
        assert!(has(&head, DRAUGR_HEAD_COLOUR));
        assert!(has(&head, DRAUGR_BANDAGE_COLOUR));
        assert!(
            has(&head, DRAUGR_EYE_COLOUR),
            "the two night-readable eyes are not part of the head mesh"
        );
        assert!(has(&arms, DRAUGR_BODY_COLOUR));
        assert!(has(&arms, DRAUGR_BANDAGE_COLOUR));

        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();
        let (arms_mesh, shared_material) = {
            let visuals = app.world().resource::<MobVisuals>();
            (
                visuals.draugr.arms.clone().expect("draugr arm mesh"),
                visuals.draugr.body_material.clone(),
            )
        };
        let world = app.world_mut();
        let mut query = world.query::<(
            &MobVisual,
            &Mesh3d,
            &MeshMaterial3d<StandardMaterial>,
            &Transform,
        )>();
        let drawn: Vec<_> = query.iter(world).collect();
        assert_eq!(
            drawn.len(),
            3,
            "the new rig costs more than body, head and one arm child"
        );
        let (_, mesh, material, transform) = drawn
            .iter()
            .find(|(visual, _, _, _)| visual.part == MobPart::Arms)
            .expect("one independently posed arm child");
        assert_eq!(mesh.0, arms_mesh);
        assert_eq!(material.0, shared_material);
        assert_eq!(transform.translation, Vec3::Y * DRAUGR_SHOULDER_HEIGHT);
    }

    #[test]
    fn only_full_screen_ui_modes_hide_bodies_without_despawning_them() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![
                targeting(draugr(900, 3.0, 60, MobAction::Idle), 7),
                vargr(901, 6.0, 35, MobAction::Chase),
            ],
        );
        app.update();

        for mode in [InputMode::Chat, InputMode::Loot, InputMode::Vendor] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Visibility, With<Mob>>();
            assert!(
                query
                    .iter(world)
                    .all(|visibility| *visibility == Visibility::Visible),
                "the centred {mode:?} panel hid the world"
            );
        }

        for mode in [InputMode::Inventory, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();

            let world = app.world_mut();
            let mut query = world.query_filtered::<&Visibility, With<Mob>>();
            let drawn: Vec<_> = query.iter(world).copied().collect();
            assert_eq!(
                drawn,
                vec![Visibility::Hidden; 2],
                "the full-screen {mode:?} mode left a body drawn"
            );
            assert_eq!(bodies(&mut app).len(), 2, "hiding despawned a body");
            assert_eq!(
                marker_owners(&mut app),
                vec![(900, Visibility::Inherited)],
                "the marker must inherit the hidden body rather than draw independently"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // The vargr
    // ---------------------------------------------------------------------------

    /// **The test the issue exists for, and the reason it was urgent.**
    ///
    /// Until legacy PR 172 a vargr never reached this module at all: `MobKind::from_wire` answered
    /// `None`, `mob_state` turned that into `DecodeError::UnknownMobEnum`, and
    /// `net/session.rs` turns any decode error into a `protocol_failure` — so the first
    /// vargr the server sent ended the connection rather than being skipped. The decoder's
    /// half is pinned in `net/codec.rs`; this is the half that makes accepting it correct,
    /// because a kind the decoder admits and the renderer cannot draw is a body with no
    /// mesh.
    #[test]
    fn a_snapshot_with_a_vargr_spawns_one_body_and_omitting_it_takes_it_away() {
        let mut app = headless();
        deliver(&mut app, 1, vec![vargr(900, 3.0, 35, MobAction::Chase)]);
        app.update();
        assert_eq!(kinds(&mut app), vec![(900, MobKind::Vargr)]);

        // The newest snapshot is the complete existence set for a vargr exactly as it is
        // for a draugr. Why it went is not asked.
        deliver(&mut app, 2, vec![]);
        app.update();
        assert!(bodies(&mut app).is_empty());
    }

    /// Two kinds in one snapshot, each drawn from its own meshes and its own colours.
    ///
    /// The parts are compared as *handles*, which is what catches the failure worth
    /// catching: a vargr silently built from the draugr's meshes would spawn the right
    /// number of children, be the right size in every component, and draw a corpse on two
    /// legs.
    ///
    /// The counts differ because the species do: a vargr's legs are a child of their own,
    /// so that a collapse can splay them, and a draugr's are part of its torso because
    /// nothing ever moves them separately.
    #[test]
    fn a_vargr_is_drawn_from_its_own_meshes_rather_than_the_draugrs() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();
        let drawn_draugr = parts(&mut app);
        assert_eq!(
            drawn_draugr.len(),
            3,
            "a draugr is a body, a head and one paired-arm child"
        );

        deliver(&mut app, 2, vec![vargr(901, 9.0, 35, MobAction::Idle)]);
        app.update();
        let drawn_vargr = parts(&mut app);
        assert_eq!(
            drawn_vargr.len(),
            4,
            "a vargr is a body, a head, its legs and one paired-eye child"
        );

        for part in &drawn_vargr {
            assert!(
                !drawn_draugr.contains(part),
                "a vargr reused one of the draugr's parts: {part:?}"
            );
        }
    }

    #[test]
    fn a_vargrs_coat_fangs_and_eyes_are_geometry_with_bounded_draw_cost() {
        fn colours(mesh: &Mesh) -> &[[f32; 4]] {
            let Some(VertexAttributeValues::Float32x4(colours)) =
                mesh.attribute(Mesh::ATTRIBUTE_COLOR)
            else {
                panic!("vargr geometry has no vertex colours");
            };
            colours
        }

        fn has(mesh: &Mesh, colour: Color) -> bool {
            colours(mesh).contains(&colour.to_linear().to_f32_array())
        }

        let body = vargr_body_mesh();
        let head = vargr_head_mesh();
        assert!(has(&body, VARGR_BODY_COLOUR));
        assert!(
            has(&body, VARGR_FUR_COLOUR),
            "the broken coat is not carried by the body draw"
        );
        assert!(has(&head, VARGR_HEAD_COLOUR));
        assert!(has(&head, VARGR_FUR_COLOUR));
        assert!(
            has(&head, VARGR_FANG_COLOUR),
            "the fangs are not carried by the head draw"
        );

        let mut app = headless();
        deliver(&mut app, 1, vec![vargr(900, 3.0, 35, MobAction::Idle)]);
        app.update();
        let drawn = parts(&mut app);
        assert_eq!(drawn.len(), 4, "the new face cost more than one child draw");

        let visuals = app.world().resource::<MobVisuals>();
        let eyes = visuals.vargr.eyes.as_ref().expect("vargr eye visuals");
        assert_eq!(visuals.vargr.body_material, visuals.vargr.head_material);
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        let eye_material = materials.get(&eyes.material).expect("eye material is live");
        assert_eq!(eye_material.base_color, VARGR_EYE_COLOUR);
        assert_eq!(eye_material.emissive, VARGR_EYE_EMISSIVE);
    }

    #[test]
    fn the_lootable_wash_is_brighter_but_still_restrained() {
        fn relative_luminance(colour: Color) -> f32 {
            let linear = colour.to_linear();
            0.2126 * linear.red + 0.7152 * linear.green + 0.0722 * linear.blue
        }

        let previous = relative_luminance(Color::srgb(0.58, 0.47, 0.24));
        let current = relative_luminance(LOOTABLE_COLOUR);
        let increase = current / previous - 1.0;

        assert_eq!(LOOTABLE_COLOUR, Color::srgb(0.64, 0.52, 0.27));
        assert!(
            (0.12..=0.30).contains(&increase),
            "the lootable wash changed relative luminance by {:.1}%, want 12-30%",
            increase * 100.0
        );
    }

    #[test]
    fn a_vargrs_eyes_stay_lit_while_every_other_part_flashes_or_turns_amber() {
        fn assert_materials(
            app: &mut App,
            state: &str,
            ordinary: &Handle<StandardMaterial>,
            eyes: &Handle<StandardMaterial>,
        ) {
            let world = app.world_mut();
            let mut query = world.query::<(&MobVisual, &MeshMaterial3d<StandardMaterial>)>();
            let drawn: Vec<_> = query
                .iter(world)
                .map(|(part, material)| (part.part, material.0.clone()))
                .collect();
            assert_eq!(drawn.len(), 4, "the vargr lost a visual part");
            for (part, material) in drawn {
                if part == MobPart::Eyes {
                    assert_eq!(
                        material, *eyes,
                        "the eyes lost their emissive material during {state}"
                    );
                } else {
                    assert_eq!(material, *ordinary, "{part:?} missed the {state} body wash");
                }
            }
        }

        let mut app = headless();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));
        deliver(&mut app, 1, vec![vargr(900, 3.0, 35, MobAction::Idle)]);
        app.update();
        let (eye_material, flash_material, lootable_material) = {
            let visuals = app.world().resource::<MobVisuals>();
            (
                visuals
                    .vargr
                    .eyes
                    .as_ref()
                    .expect("vargr eye visuals")
                    .material
                    .clone(),
                visuals.flash_material.clone(),
                visuals.lootable_material.clone(),
            )
        };

        // Give the one-tick-delayed sampler an unchanged second endpoint before the hit.
        deliver(&mut app, 2, vec![vargr(900, 3.0, 35, MobAction::Chase)]);
        app.update();
        deliver(&mut app, 3, vec![vargr(900, 3.0, 20, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 1, "the hit did not begin a flash");
        assert_materials(&mut app, "flash", &flash_material, &eye_material);

        app.insert_resource(TimeUpdateStrategy::ManualDuration(FLASH_TIME));
        app.update();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));
        deliver_lootable(&mut app, 4, vargr(900, 3.0, 0, MobAction::Corpse), true);
        app.update();
        deliver_lootable(&mut app, 5, vargr(900, 3.0, 0, MobAction::Corpse), true);
        app.update();
        // The killing decrease flashes first; amber is the steady corpse state underneath.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(FLASH_TIME));
        app.update();
        assert_materials(&mut app, "lootable", &lootable_material, &eye_material);
    }

    // -----------------------------------------------------------------------
    // Falling over
    // -----------------------------------------------------------------------

    /// The rotation a body is actually drawn with, which is where a pose has to show.
    fn drawn_rotation(app: &mut App) -> Quat {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Transform, With<Mob>>();
        query.iter(world).next().expect("a body was drawn").rotation
    }

    /// The scale on the legs child, or `None` for a species drawn without one.
    fn drawn_leg_scale(app: &mut App) -> Option<Vec3> {
        let world = app.world_mut();
        let mut query = world.query::<(&MobVisual, &Transform)>();
        query
            .iter(world)
            .find(|(part, _)| part.part == MobPart::Legs)
            .map(|(_, transform)| transform.scale)
    }

    /// One frame of local time, short enough that a fall barely moves in it.
    ///
    /// The same millisecond [`let_the_body_land`] leaves the clock on, so a test that lands
    /// a body and then asserts about the next frame is reading one unit either way.
    const ONE_FRAME: Duration = Duration::from_millis(1);

    /// How far through its fall the one body in the world is, or `None` if it is not going
    /// down. Read off the component rather than inferred from the drawn rotation, because
    /// what is being asserted is where the fall *started* and two different starts can draw
    /// the same first frame.
    fn falling(app: &mut App) -> Option<Duration> {
        let world = app.world_mut();
        let mut query = world.query::<&Mob>();
        query.single(world).expect("one body").falling
    }

    /// How long each step of a manual fall takes.
    ///
    /// **Comfortably under `Time<Virtual>`'s default `max_delta` of 250 ms**, which is the
    /// trap this constant exists to avoid: a single `ManualDuration(FALL_TIME)` looks like
    /// it advances 700 ms and advances 250, so a fall driven in one step arrives about a
    /// sixth of the way over and every assertion about where it ended up is really an
    /// assertion about the clamp. [`FLASH_TIME`] is under the clamp, which is why the
    /// flash test above never had to know.
    const FALL_STEP: Duration = Duration::from_millis(100);

    /// Steps local time far enough for any fall to have finished, and then puts the clock
    /// back to something a later assertion can hold still with.
    fn let_the_body_land(app: &mut App) {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(FALL_STEP));
        for _ in 0..(FALL_TIME.div_duration_f32(FALL_STEP).ceil() as u32 + 1) {
            app.update();
        }
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));
    }

    /// A draugr goes over **backwards**, and the assertion is in world space.
    ///
    /// **The sign is the whole test.** `DRAUGR_FALL_PITCH` is positive because the meshes
    /// face -Z and a positive rotation about +X carries the top of a body towards +Z, and
    /// that is two conventions multiplied together — exactly the kind of reasoning that is
    /// right until somebody re-authors a mesh. So the claim is made about where the body's
    /// own up axis ends up pointing rather than about the constant: a draugr at rest points
    /// up, and a fallen one points the way it came from.
    #[test]
    fn a_draugr_goes_over_backwards() {
        let mut app = headless();
        // Yaw 0, so the creature's local frame is the world's and "behind" is +Z.
        deliver(&mut app, 1, vec![draugr(900, 0.0, 60, MobAction::Chase)]);
        app.update();
        let upright = drawn_rotation(&mut app) * Vec3::Y;
        assert!(
            upright.dot(Vec3::Y) > 0.99,
            "a living draugr is already leaning: its up axis is {upright}"
        );

        deliver(&mut app, 2, vec![draugr(900, 0.0, 0, MobAction::Dying)]);
        app.update();
        let_the_body_land(&mut app);

        let fallen = drawn_rotation(&mut app) * Vec3::Y;
        assert!(
            fallen.z > 0.99,
            "a draugr that fell over ended up with its head at {fallen}, want it behind at +Z"
        );

        let world = app.world_mut();
        let mut arms = world.query::<(&MobVisual, &Transform)>();
        let (_, arms) = arms
            .iter(world)
            .find(|(part, _)| part.part == MobPart::Arms)
            .expect("one arm child");
        assert_eq!(
            arms.rotation,
            Quat::IDENTITY,
            "the arm telegraph fought the parent's backwards fall"
        );
        assert_eq!(arms.translation, Vec3::Y * DRAUGR_SHOULDER_HEIGHT);
    }

    #[test]
    fn dying_becomes_a_highlighted_corpse_without_restarting_or_replacing_the_body() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 0.0, 0, MobAction::Dying)]);
        app.update();
        let_the_body_land(&mut app);
        let before = {
            let world = app.world_mut();
            let mut query = world.query_filtered::<Entity, With<Mob>>();
            query.single(world).expect("one dying body")
        };

        deliver_lootable(&mut app, 2, draugr(900, 0.0, 0, MobAction::Corpse), true);
        app.update();
        let (after, lootable) = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &Mob)>();
            let (entity, mob) = query.single(world).expect("one corpse");
            (entity, mob.lootable)
        };
        assert_eq!(after, before, "Dying -> Corpse replaced the visual entity");
        assert!(lootable);
        assert!((drawn_rotation(&mut app) * Vec3::Y).z > 0.99);

        let highlight = app
            .world()
            .resource::<MobVisuals>()
            .lootable_material
            .clone();
        assert!(
            parts(&mut app)
                .iter()
                .all(|(_, material)| *material == highlight)
        );

        deliver_lootable(&mut app, 3, draugr(900, 0.0, 0, MobAction::Corpse), false);
        app.update();
        assert!(!{
            let world = app.world_mut();
            let mut query = world.query::<&Mob>();
            query.single(world).expect("one corpse").lootable
        });
    }

    /// A vargr collapses sideways with its legs sliding out from under it.
    ///
    /// Both halves, because either alone is a different animation: a body that rolls with
    /// its legs still tucked under it is a carcass somebody pushed, and legs that splay
    /// under a body still standing is a stance rather than a death.
    #[test]
    fn a_vargr_collapses_with_its_legs_splaying_out() {
        let mut app = headless();
        deliver(&mut app, 1, vec![vargr(901, 0.0, 35, MobAction::Chase)]);
        app.update();
        assert_eq!(
            drawn_leg_scale(&mut app),
            Some(Vec3::ONE),
            "a living vargr already has its legs out"
        );

        deliver(&mut app, 2, vec![vargr(901, 0.0, 0, MobAction::Dying)]);
        app.update();
        let_the_body_land(&mut app);

        // Sideways: the up axis has gone over towards one flank rather than fore or aft,
        // which is what tells this apart from the draugr's topple.
        let fallen = drawn_rotation(&mut app) * Vec3::Y;
        assert!(
            fallen.x.abs() > 0.9 && fallen.z.abs() < 0.1,
            "a vargr's collapse left its up axis at {fallen}, want it over on one side"
        );

        let splayed = drawn_leg_scale(&mut app).expect("a vargr is drawn with legs");
        assert!(
            (splayed.x - VARGR_LEG_SPLAY).abs() < 1e-4
                && (splayed.z - VARGR_LEG_SPLAY).abs() < 1e-4,
            "the legs finished at {splayed}, want them out to {VARGR_LEG_SPLAY}"
        );
        assert!(
            (splayed.y - 1.0).abs() < 1e-6,
            "the collapse stretched the legs vertically to {}",
            splayed.y
        );
    }

    /// **Zero health is still not death**, and the fall is the proof.
    ///
    /// The module's oldest rule, asserted against the one animation that could break it:
    /// a body sent with no health left and an action that is not `Dying` stands there. It
    /// is not a state the server produces today — health reaches zero and the action
    /// changes in the same breath — and that is exactly why it is worth pinning, because
    /// the tempting shortcut is to read the number that is already in the struct instead of
    /// the field that says what happened.
    #[test]
    fn no_health_left_is_not_a_reason_to_fall_over() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 0.0, 0, MobAction::Chase)]);
        app.update();
        let_the_body_land(&mut app);

        let standing = drawn_rotation(&mut app) * Vec3::Y;
        assert!(
            standing.dot(Vec3::Y) > 0.99,
            "a draugr with no health the server never called dying fell over anyway: {standing}"
        );
    }

    /// A fall is a pose and never a clock: the body goes when the *server* stops sending
    /// it, however long it has been lying there.
    ///
    /// This is what keeps `FALL_TIME` presentation with nothing resting on it. A fall that
    /// finished and then despawned its own body would be this client deciding when a
    /// creature stopped existing — and it would be visibly wrong against any corpse lifetime
    /// but the one it happened to be tuned against.
    #[test]
    fn a_body_lies_where_it_landed_until_the_server_stops_sending_it() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 0.0, 0, MobAction::Dying)]);
        app.update();
        let_the_body_land(&mut app);

        // Four more falls' worth of local time, with the server still naming it.
        for tick in 2..6 {
            deliver(&mut app, tick, vec![draugr(900, 0.0, 0, MobAction::Dying)]);
            let_the_body_land(&mut app);
        }
        assert_eq!(
            bodies(&mut app),
            vec![(900, 0, MobAction::Dying)],
            "the body went before the server said so"
        );
        let lying = drawn_rotation(&mut app) * Vec3::Y;
        assert!(
            lying.z > 0.99,
            "the body kept turning past the end of its fall: {lying}"
        );

        // And then the server stops naming it, which is the only thing that ends a death.
        deliver(&mut app, 6, vec![]);
        app.update();
        assert!(
            bodies(&mut app).is_empty(),
            "the body outlived the snapshot"
        );
    }

    /// **A creature that was alive in the last snapshot starts its fall from upright, on
    /// the snapshot that first calls it a corpse.**
    ///
    /// This is the client half of #441. The server used to send `Dying` for two and a half
    /// seconds and only then `Corpse`, so by the time a body was called a corpse its fall
    /// had long finished and the `Corpse` arm was right to start it already landed. The
    /// killing blow now produces the corpse directly: the snapshot that says `Corpse` is
    /// the one that used to say `Dying`, and a body that carried on standing upright and
    /// then snapped flat would be the visible cost of not moving with it.
    ///
    /// The lootable flag is asserted on the same frame rather than in a test of its own,
    /// because "the body is going down" and "the body can be looted" are now one transition
    /// and the whole point is that a player does not have to wait out the first to get the
    /// second. The *material* is the impact flash for the first [`FLASH_TIME`], because the
    /// killing blow is a health decrease like any other and this is the frame it lands on;
    /// the amber is asserted once that has run out, which is what a player sees.
    #[test]
    fn a_body_that_was_alive_starts_its_fall_on_the_snapshot_that_says_corpse() {
        let mut app = headless();
        // A frame of known length, because the system that reads the snapshot is the one
        // that advances the pose: whatever the fall starts at, one frame of it has already
        // been spent by the time the assertion below can see it. A millisecond makes the
        // difference between the two possible starts — zero and FALL_TIME — unmistakable.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(ONE_FRAME));

        deliver(&mut app, 1, vec![draugr(900, 0.0, 60, MobAction::Chase)]);
        app.update();
        assert_eq!(falling(&mut app), None, "a living draugr is going down");

        deliver_lootable(&mut app, 2, draugr(900, 0.0, 0, MobAction::Corpse), true);
        app.update();
        assert_eq!(
            falling(&mut app),
            Some(ONE_FRAME),
            "the fall did not start from upright on the tick of the kill"
        );
        let upright = drawn_rotation(&mut app) * Vec3::Y;
        assert!(
            upright.dot(Vec3::Y) > 0.99,
            "the body was already over on the tick it died: {upright}"
        );

        assert!(
            {
                let world = app.world_mut();
                let mut query = world.query::<&Mob>();
                query.single(world).expect("one body").lootable
            },
            "the body is not lootable on the tick it died"
        );

        // And it is a fall rather than a value that sits at zero: local time carries it,
        // and it finishes flat, tinted, with the impact flash long over.
        let_the_body_land(&mut app);
        assert!(
            falling(&mut app).is_some_and(|elapsed| elapsed >= FALL_TIME),
            "the fall did not advance"
        );
        let fallen = drawn_rotation(&mut app) * Vec3::Y;
        assert!(
            fallen.z > 0.99,
            "the body did not finish its fall: its up axis is {fallen}"
        );
        let highlight = app
            .world()
            .resource::<MobVisuals>()
            .lootable_material
            .clone();
        assert!(
            parts(&mut app)
                .iter()
                .all(|(_, material)| *material == highlight),
            "the fallen body is not tinted as lootable"
        );
    }

    /// **A body first *seen* as a corpse is already lying flat**, and it is the only one.
    ///
    /// A corpse that has been there since before this client could draw it must not stand
    /// up in order to fall over, and the wire carries no elapsed time to tell it how far
    /// through it should be. The two cases are told apart by which code path reaches them —
    /// `spawn_mob` has no previous snapshot — rather than by a remembered action, so this
    /// pins the half that has no `Mob` to have been alive in.
    #[test]
    fn a_body_first_seen_as_a_corpse_is_already_lying_flat() {
        let mut app = headless();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(ONE_FRAME));
        deliver_lootable(&mut app, 1, draugr(900, 0.0, 0, MobAction::Corpse), true);
        app.update();

        // Exactly FALL_TIME rather than a frame past it: spawning is a deferred command,
        // so the pose system does not see this body until the frame after the one that
        // created it. What is asserted is the value spawn_mob chose.
        assert_eq!(
            falling(&mut app),
            Some(FALL_TIME),
            "a corpse streamed into view stood back up to fall over"
        );
        let lying = drawn_rotation(&mut app) * Vec3::Y;
        assert!(
            lying.z > 0.99,
            "a corpse streamed into view is drawn upright: {lying}"
        );
    }

    /// The fall accelerates and it ends, which is what makes it a topple rather than a
    /// rotation at a constant rate that never arrives.
    #[test]
    fn the_fall_starts_slowly_and_stops_when_it_is_over() {
        assert_eq!(fallen(Duration::ZERO), 0.0);
        assert_eq!(fallen(FALL_TIME), 1.0);
        assert_eq!(
            fallen(FALL_TIME * 10),
            1.0,
            "a body kept falling past the end of its own animation"
        );

        let halfway = fallen(FALL_TIME / 2);
        assert!(
            halfway < 0.5,
            "the fall was half over at half the time ({halfway}), so it does not accelerate"
        );
    }

    /// **The drawn body is the box the server collides**, for every kind, exactly.
    ///
    /// The numbers are mirrored from the `body` field of each row in
    /// `server/internal/game/species.go`, which is what the server's collision, its
    /// swing reach and its spawn separation all read. A drawn body smaller than that box
    /// is a creature a swing reaches through empty air; a larger one is a creature that
    /// visibly overlaps ground it does not occupy. Both are the same defect the
    /// `PLAYER_WIDTH`/`PLAYER_HEIGHT` mirror exists to prevent.
    ///
    /// Asserted on the *meshes* rather than on the constants, so a part authored at the
    /// wrong offset fails here rather than looking right in a table and wrong on screen.
    ///
    /// **Every mesh a species is drawn from goes into the list**, which is what caught the
    /// vargr's legs moving into a child of their own: taking them out of the body left the
    /// box 0.83 tall against the 1.0 the server collides, and the list is the only thing
    /// that noticed. The extents are measured at rest, before any collapse has splayed
    /// anything — a body on its way over is deliberately outside the box it stood in.
    #[test]
    fn the_drawn_body_is_the_box_the_server_collides() {
        let drawn = [
            (
                MobKind::Draugr,
                vec![
                    draugr_body_mesh(),
                    draugr_head_mesh(),
                    draugr_arms_mesh().translated_by(Vec3::Y * DRAUGR_SHOULDER_HEIGHT),
                ],
            ),
            (
                MobKind::Vargr,
                vec![
                    vargr_body_mesh(),
                    vargr_head_mesh(),
                    vargr_legs_mesh(),
                    vargr_eye_mesh(),
                ],
            ),
            (
                MobKind::Deer,
                vec![deer_body_mesh(), deer_head_mesh(), deer_legs_mesh()],
            ),
        ];

        // The list above is hand-written, for the reason every list like it in this
        // repository is: one derived from the same `match` it checks would agree with
        // every hole in that `match`. But hand-written is exactly how V25's villager got
        // as far as a merged pull request with all four of its arms untested — a `for`
        // over an array does not stop compiling when the enum grows. So the length is
        // pinned to the contract's own count, the way `EVERY_REASON` and the codec's
        // `CLASSIFICATION` are.
        //
        // **Three members have no row, for separate reasons.** `Unknown` is never drawn at
        // all: `MobKind::from_wire` answers `None` for it. `Villager` *is* drawn, by the
        // humanoid rig in `player/mod.rs` rather than by any mesh here, so its box is
        // checked in [`a_villagers_box_is_the_one_the_server_collides_for_a_person`].
        // `Horse` is drawn through the shared ridden-horse rig in `player/horse.rs`; that
        // module pins horse, tack and rider to the mounted body the server collides. It
        // has no server collision box of its own because paddock horses are not mobs.
        assert_eq!(
            drawn.len(),
            crate::wire::voxelheim::net::MobKind::ENUM_VALUES.len() - 3,
            "a species the contract names is drawn by nobody here, so its box is unchecked"
        );
        for (seen, (kind, _)) in drawn.iter().enumerate() {
            assert!(
                !drawn[..seen].iter().any(|(other, _)| other == kind),
                "{kind:?} appears twice, so some other species is absent"
            );
        }

        for (kind, meshes) in drawn {
            let expected = body(kind);
            let (min, max) = drawn_extent(&meshes);
            let half = expected.width / 2.0;

            for (axis, got, want) in [
                ("width", max.x - min.x, expected.width),
                ("height", max.y - min.y, expected.height),
                ("depth", max.z - min.z, expected.width),
            ] {
                assert!(
                    (got - want).abs() < 1e-5,
                    "a {kind:?} is drawn {got} across in {axis}, want the {want} the server collides"
                );
            }

            // And it is centred on the point the snapshot names, standing on it: the
            // server sends the feet, so a body floating or sunk is the same mismatch one
            // axis over.
            assert!(min.y.abs() < 1e-5, "a {kind:?} does not stand on its feet");
            assert!(
                (min.x + half).abs() < 1e-5 && (max.x - half).abs() < 1e-5,
                "a {kind:?} is not centred on the position the server sends"
            );
        }
    }

    #[test]
    fn the_vargrs_ruff_reaches_the_collided_height_and_its_face_stays_inside_the_box() {
        let vargr = body(MobKind::Vargr);
        let (_, body_max) = drawn_extent(&[vargr_body_mesh()]);
        assert!(
            (body_max.y - vargr.height).abs() < 1e-5,
            "the ruff reaches {} high, want {}",
            body_max.y,
            vargr.height
        );

        let (head_min, head_max) = drawn_extent(&[vargr_head_mesh(), vargr_eye_mesh()]);
        let half = vargr.width / 2.0;
        let epsilon = 1e-5;
        assert!(head_min.x >= -half - epsilon && head_max.x <= half + epsilon);
        assert!(head_min.y >= -epsilon && head_max.y <= vargr.height + epsilon);
        assert!(
            head_min.z >= -half - epsilon && head_max.z <= half + epsilon,
            "the eyes or fangs left the head end of the collision box: {head_min}..{head_max}"
        );
    }

    /// A villager's box is the server's, mirrored the way a player's is.
    ///
    /// The row [`the_drawn_body_is_the_box_the_server_collides`] cannot carry, because that
    /// sweep measures meshes and nothing here draws a resident. What is left is the mirror
    /// itself: `residentBody` in `server/internal/game/resident.go` is
    /// `{PlayerWidth, PlayerHeight}`, and this side's copy has to be the client's copy of
    /// those two rather than a second pair of literals that happen to agree today.
    ///
    /// **There is deliberately no comparison against the rig's own extent.** The humanoid
    /// rig legitimately overflows the box it stands in — a topknot reaches 1.975 against a
    /// 1.8 box, outstretched fists span 0.8 against a 0.6 one — and that latitude is the
    /// *player's*, unchanged by this issue.
    #[test]
    fn a_villagers_box_is_the_one_the_server_collides_for_a_person() {
        let villager = body(MobKind::Villager);
        assert_eq!(villager.width, crate::player::constants::PLAYER_WIDTH);
        assert_eq!(villager.height, crate::player::constants::PLAYER_HEIGHT);
        assert!(
            villager.height > villager.width,
            "a resident stopped being an upright figure"
        );
    }

    /// This module draws no villager, and says so where a caller can read it.
    ///
    /// The half a box comparison cannot see: a resident could have exactly the right box
    /// and still be spawned as two grey cuboids. It used to be — until #458 `of` answered
    /// `&self.draugr`, the honest placeholder while no server sent a villager.
    #[test]
    fn a_villager_is_drawn_by_the_humanoid_rig_and_not_by_this_module() {
        let mut app = headless();
        app.update();
        let world = app.world();
        let visuals = world.resource::<MobVisuals>();

        assert!(
            visuals.of(MobKind::Villager).is_none(),
            "this module offers meshes for a resident again; residents are people and are \
             drawn through `spawn_body` on the humanoid rig"
        );
        assert!(
            visuals.of(MobKind::Horse).is_none(),
            "paddock horses must use the shared horse rig rather than generic mob meshes"
        );
        for creature in [MobKind::Draugr, MobKind::Vargr, MobKind::Deer] {
            assert!(
                visuals.of(creature).is_some(),
                "{creature:?} has no visuals, so nothing draws it"
            );
        }
    }

    /// A villager in the snapshot leaves no creature behind in this module.
    ///
    /// The routing observed rather than asserted about: one `MobState` vector carries both,
    /// and after a frame there is a `Mob` for the draugr and none for the villager. The
    /// aggro marker, the tint and the fall need no test of their own — all three hang off
    /// the component that was never created.
    #[test]
    fn a_villager_in_the_snapshot_spawns_no_mob() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![draugr(1, 2.0, 60, MobAction::Idle), villager(2, 4.0)],
        );
        app.update();

        let kinds: Vec<(u64, MobKind)> = {
            let world = app.world_mut();
            let mut query = world.query::<&Mob>();
            query
                .iter(world)
                .map(|drawn| (drawn.entity_id, drawn.kind))
                .collect()
        };
        assert_eq!(
            kinds,
            vec![(1, MobKind::Draugr)],
            "a resident was drawn as a creature by the module that draws creatures"
        );
    }

    /// A vargr is unmistakable from a draugr and from a remote player's capsule.
    ///
    /// Both of those are 0.6 × 1.8 — `PLAYER_WIDTH`/`PLAYER_HEIGHT` and the draugr's row
    /// happen to agree — so "low and long" is one comparison: broader in plan and much
    /// shorter. Asserted as a *relationship* rather than as two more copies of the
    /// numbers, because what has to survive a rebalance is the silhouette being different,
    /// not either species keeping the size it has today.
    #[test]
    fn a_vargr_reads_as_neither_a_draugr_nor_a_player() {
        let vargr = body(MobKind::Vargr);
        let draugr = body(MobKind::Draugr);

        assert!(
            vargr.width > draugr.width,
            "a vargr is not broader than a draugr"
        );
        assert!(
            vargr.height < draugr.height / 1.5,
            "a vargr is not markedly shorter than a draugr"
        );
        // Taller than it is wide is what a standing figure looks like; a vargr is not one.
        assert!(
            draugr.height > draugr.width,
            "a draugr stopped being an upright figure"
        );
        assert!(
            vargr.height < vargr.width * 1.5,
            "a vargr is not low and long"
        );

        // The same shape the client draws a player's capsule at, so the third silhouette
        // in the comparison is pinned rather than remembered.
        assert_eq!(draugr.width, crate::player::constants::PLAYER_WIDTH);
        assert_eq!(draugr.height, crate::player::constants::PLAYER_HEIGHT);
    }

    /// Windup, recovery and the hit flash read on a vargr exactly as on a draugr.
    ///
    /// One code path, and this is what says so: the pose is a function of the *action*
    /// alone — the server's word — and the flash is a function of an authoritative health
    /// decrease. Neither consults the kind, and neither may, because the windup and
    /// recovery durations belong to the row in `species.go` and a second copy of them here
    /// would be a second thing to keep in step.
    #[test]
    fn windup_and_recovery_read_on_a_vargr_exactly_as_on_a_draugr() {
        let mut app = headless();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));

        deliver(&mut app, 1, vec![vargr(900, 3.0, 35, MobAction::Windup)]);
        app.update();
        assert_eq!(bodies(&mut app), vec![(900, 35, MobAction::Windup)]);
        assert_eq!(
            flashing(&mut app),
            0,
            "the first sight of a body is not a hit"
        );

        // The lean is the action's, and the action is the server's: it is held until a
        // newer snapshot changes it, whatever local time does.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(INTERVAL));
        for _ in 0..5 {
            app.update();
            assert_eq!(bodies(&mut app), vec![(900, 35, MobAction::Windup)]);
        }
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));

        deliver(&mut app, 2, vec![vargr(900, 3.0, 35, MobAction::Recovery)]);
        app.update();
        assert_eq!(bodies(&mut app), vec![(900, 35, MobAction::Recovery)]);
        assert_eq!(flashing(&mut app), 0, "an unchanged health flashed");

        // And a decrease flashes, through the one shared material swap.
        deliver(&mut app, 3, vec![vargr(900, 3.0, 12, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 1, "a vargr taking a hit did not flash");
    }

    /// Zero health is not death for a vargr either, and the two kinds coexist.
    #[test]
    fn a_snapshot_carrying_both_kinds_draws_both() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![
                draugr(900, 3.0, 60, MobAction::Idle),
                vargr(901, 9.0, 0, MobAction::Chase),
                vargr(902, 12.0, 35, MobAction::Windup),
            ],
        );
        app.update();

        assert_eq!(
            kinds(&mut app),
            vec![
                (900, MobKind::Draugr),
                (901, MobKind::Vargr),
                (902, MobKind::Vargr),
            ]
        );
        assert_eq!(
            bodies(&mut app),
            vec![
                (900, 60, MobAction::Idle),
                (901, 0, MobAction::Chase),
                (902, 35, MobAction::Windup),
            ],
            "a vargr at zero health was despawned by this client"
        );
    }
}
