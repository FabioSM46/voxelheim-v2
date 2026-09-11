//! Spell shapes inside the regions the server announced: Burial's lit cracks, the
//! Edict's rune groups, the Requiem's closing notes and the thrown Sepulchre Spear.
//! Each is sampled from the newest authoritative tick and never leaves its volume.
//! None carries information the boundary cues and readings lack, so without this layer
//! every essential cue remains; nothing here moves a camera or decides contact. That is
//! why [`ReducedEffects`] withholds the whole layer, and why switching it takes effect on
//! the next frame without waiting for a new announcement.

use std::f32::consts::TAU;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::cues::placement;
use super::{EncounterPresentation, MoveKey, PresentedMove, ReducedEffects, Window, reconcile};
use crate::net::{
    BlockCoord, EncounterMoveKind, HazardShape, HazardVolume, MobKind, MovePhase, Session,
};
use crate::world::ChunkStore;

#[cfg(test)]
mod capture;

const CRACKS: usize = 24;
const GLYPHS: usize = 6;
const ARC_STEPS: usize = 32;
const LINE: f32 = 0.05;
const SHARD: f32 = 0.10;
/// A released spear's length and half thickness, in blocks.
const SPEAR_LENGTH: f32 = 1.2;
const SPEAR_RADIUS: f32 = 0.12;
/// The server's projectile sub-step, so a wall stops the drawn spear where it stops.
const FLIGHT_STEP: f32 = 0.25;
/// Just above the boundary cue, so the two never share a plane.
const LIFT: f32 = 0.02;
/// Per effect; the inbox, timeline and hazard bounds allow 512 effects at most.
#[cfg(test)]
const MAX_VERTICES: usize = 768;

#[derive(Resource)]
struct SpellMaterials {
    gathering: Handle<StandardMaterial>,
    erupting: Handle<StandardMaterial>,
    crystal: Handle<StandardMaterial>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    /// Burial: cracks lit around the announced annulus as its pulse approaches.
    Cracks {
        inner: f32,
        outer: f32,
        lit: usize,
        erupting: bool,
        height: f32,
    },
    /// Edict: rune staves lit in order inside each sector, with the pulse's number.
    Runes {
        radius: f32,
        lit: usize,
        tally: u8,
        erupting: bool,
        height: f32,
    },
    /// Requiem: one ring per intoned note and a ring closing on the sector.
    Notes {
        radius: f32,
        notes: u8,
        closing: f32,
        erupting: bool,
        height: f32,
    },
    /// Sepulchre Spear: the crystal where the server's flight has it.
    Spear { length: f32 },
}

impl Shape {
    fn erupting(self) -> bool {
        match self {
            Self::Cracks { erupting, .. }
            | Self::Runes { erupting, .. }
            | Self::Notes { erupting, .. } => erupting,
            Self::Spear { .. } => false,
        }
    }
}

#[derive(Component)]
struct SpellEffect {
    key: MoveKey,
    index: usize,
    shape: Shape,
}

/// Test-only: the move instances whose spell shapes are drawn now.
#[cfg(test)]
pub(super) fn drawn(world: &mut World) -> std::collections::HashSet<MoveKey> {
    world
        .query::<(&SpellEffect, &InheritedVisibility)>()
        .iter(world)
        .filter(|(_, visible)| visible.get())
        .map(|(effect, _)| effect.key)
        .collect()
}

pub(super) fn register(app: &mut App) {
    app.add_systems(Startup, setup)
        .add_systems(Update, refresh.after(reconcile));
}

fn setup(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    let mut material = |colour| {
        materials.add(StandardMaterial {
            base_color: colour,
            unlit: true,
            fog_enabled: false,
            cull_mode: None,
            ..default()
        })
    };
    commands.insert_resource(SpellMaterials {
        gathering: material(Color::srgb(0.42, 0.78, 0.95)),
        erupting: material(Color::srgb(0.90, 0.98, 1.0)),
        crystal: material(Color::srgb(0.62, 0.90, 1.0)),
    });
}

#[allow(clippy::type_complexity)] // One query per effect, matching the boundary cues.
#[allow(clippy::too_many_arguments)] // A system's inputs are its parameters; each is read here.
fn refresh(
    mut commands: Commands,
    presentation: Res<EncounterPresentation>,
    reduced: Option<Res<ReducedEffects>>,
    materials: Res<SpellMaterials>,
    session: Option<Res<Session>>,
    store: Option<Res<ChunkStore>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut effects: Query<(
        Entity,
        &mut SpellEffect,
        &Mesh3d,
        &mut MeshMaterial3d<StandardMaterial>,
        &mut Transform,
    )>,
) {
    // A switched setting is a change too: the encounter must not have to announce again.
    if !presentation.is_changed() && !reduced.as_ref().is_some_and(|reduced| reduced.is_changed()) {
        return;
    }
    let reduced = reduced.is_some_and(|reduced| reduced.0);
    let solid = |voxel: IVec3| {
        session
            .as_ref()
            .zip(store.as_ref())
            .is_some_and(|(session, store)| {
                store.solid_at(
                    BlockCoord {
                        x: voxel.x,
                        y: voxel.y,
                        z: voxel.z,
                    },
                    session.0.chunk_size as usize,
                )
            })
    };
    let mut wanted = Vec::new();
    // Withheld means wanting nothing, so the loop below releases every drawn mesh.
    for one in presentation.0.iter().filter(|_| !reduced) {
        for (index, volume) in one.hazards().iter().enumerate() {
            if let Some((shape, transform)) = effect_for(one, volume, &solid) {
                wanted.push((one.key, index, shape, transform));
            }
        }
    }
    let material = |shape: Shape| match shape {
        Shape::Spear { .. } => materials.crystal.clone(),
        shape if shape.erupting() => materials.erupting.clone(),
        _ => materials.gathering.clone(),
    };
    let mut held = Vec::with_capacity(wanted.len());
    for (entity, mut effect, mesh, mut current, mut placed) in &mut effects {
        let Some(&(_, _, shape, transform)) = wanted
            .iter()
            .find(|(key, index, ..)| *key == effect.key && *index == effect.index)
        else {
            meshes.remove(mesh.0.id());
            commands.entity(entity).despawn();
            continue;
        };
        if effect.shape != shape {
            if let Some(mut asset) = meshes.get_mut(&mesh.0) {
                *asset = shape_mesh(shape);
            }
            effect.shape = shape;
        }
        let next = material(shape);
        if current.0 != next {
            current.0 = next;
        }
        *placed = transform;
        held.push((effect.key, effect.index));
    }
    for (key, index, shape, transform) in wanted {
        if held.contains(&(key, index)) {
            continue;
        }
        commands.spawn((
            SpellEffect { key, index, shape },
            Mesh3d(meshes.add(shape_mesh(shape))),
            MeshMaterial3d(material(shape)),
            transform,
        ));
    }
}

/// The share of this phase's ticks that have run, including the presented one.
fn fraction(one: &PresentedMove) -> f32 {
    let ticks = one.announced.phase_ticks.max(1);
    (ticks.saturating_sub(one.remaining_ticks) + 1).min(ticks) as f32 / ticks as f32
}

fn lit(count: usize, fraction: f32) -> usize {
    ((count as f32 * fraction).ceil() as usize).clamp(1, count)
}

fn effect_for(
    one: &PresentedMove,
    volume: &HazardVolume,
    solid: &dyn Fn(IVec3) -> bool,
) -> Option<(Shape, Transform)> {
    if one.boss_kind != MobKind::DraugrKing
        || one.window != Window::Current
        || one.announced.ended.is_some()
    {
        return None;
    }
    let t = fraction(one);
    let erupting = one.damaging();
    let height = volume.height;
    let pulse = one
        .announced
        .pulse
        .map_or(0, |(index, _)| index.saturating_add(1));
    let mut floor = placement(*volume, one.boss_kind);
    floor.translation.y += LIFT;
    let ritual = matches!(
        one.announced.phase,
        MovePhase::Telegraph | MovePhase::Channel
    );
    let shape = match (one.announced.kind, volume.shape) {
        (EncounterMoveKind::Burial, HazardShape::Ring { inner_radius }) if ritual => {
            Shape::Cracks {
                inner: inner_radius,
                outer: volume.radius,
                lit: lit(CRACKS, t),
                erupting,
                height,
            }
        }
        (EncounterMoveKind::EdictOfTheGraves, HazardShape::Disc) if ritual => Shape::Runes {
            radius: volume.radius,
            lit: lit(GLYPHS, t),
            tally: pulse.min(8),
            erupting,
            height,
        },
        (EncounterMoveKind::RequiemOfTheBuried, HazardShape::Disc) if ritual => Shape::Notes {
            radius: volume.radius,
            notes: pulse.min(4),
            closing: t,
            erupting,
            height,
        },
        (EncounterMoveKind::SepulchreSpear, HazardShape::Line { .. })
            if one.announced.phase == MovePhase::Release =>
        {
            let direction = Vec3::from_array(volume.direction)
                .with_y(0.0)
                .normalize_or_zero();
            let reached = flight(one, volume, solid);
            let length = reached.min(SPEAR_LENGTH);
            if direction == Vec3::ZERO || length < 0.05 {
                return None;
            }
            let centre = Vec3::from_array(volume.origin) + direction * (reached - length / 2.0);
            return Some((
                Shape::Spear { length },
                Transform::from_translation(centre)
                    .with_rotation(Quat::from_rotation_arc(Vec3::Z, direction)),
            ));
        }
        _ => return None,
    };
    Some((shape, floor))
}

/// How far along its locked lane the spear is: the server crosses the whole lane in the
/// release's ticks, one equal step per tick, and stops before the first solid voxel.
fn flight(one: &PresentedMove, volume: &HazardVolume, solid: &dyn Fn(IVec3) -> bool) -> f32 {
    let ticks = one.announced.phase_ticks.max(1);
    let crossed = (ticks.saturating_sub(one.remaining_ticks) + 1).min(ticks);
    let wanted = volume.radius * crossed as f32 / ticks as f32;
    let origin = Vec3::from_array(volume.origin);
    let direction = Vec3::from_array(volume.direction)
        .with_y(0.0)
        .normalize_or_zero();
    let mut reached = 0.0;
    while reached < wanted {
        let next = (reached + FLIGHT_STEP).min(wanted);
        if solid((origin + direction * next).floor().as_ivec3()) {
            break;
        }
        reached = next;
    }
    reached
}

pub(super) fn polar(angle: f32, radius: f32) -> Vec2 {
    Vec2::new(angle.cos(), angle.sin()) * radius
}

fn shard_height(height: f32) -> f32 {
    (height * 0.5).min(0.9)
}

/// Flat floor strips and raised quads, shared with the strike layer.
#[derive(Default)]
pub(super) struct Builder {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

impl Builder {
    pub(super) fn quad(&mut self, corners: [Vec3; 4]) {
        let start = self.positions.len() as u32;
        self.positions.extend(corners.map(|v| v.to_array()));
        self.indices
            .extend([start, start + 2, start + 1, start, start + 3, start + 2]);
    }

    pub(super) fn strip(&mut self, a: Vec2, b: Vec2, width: f32) {
        let side = (b - a).normalize_or_zero().perp() * width / 2.0;
        self.quad([a - side, b - side, b + side, a + side].map(|p| Vec3::new(p.x, 0.0, p.y)));
    }

    fn circle(&mut self, radius: f32, width: f32) {
        for step in 0..ARC_STEPS {
            let angle = |step: usize| TAU * step as f32 / ARC_STEPS as f32;
            self.strip(
                polar(angle(step), radius),
                polar(angle(step + 1), radius),
                width,
            );
        }
    }

    /// Two crossed upright quads: the one raised element, shown only on contact.
    fn shard(&mut self, at: Vec2, height: f32) {
        for axis in [Vec2::X, Vec2::Y] {
            let (a, b) = (at - axis * SHARD / 2.0, at + axis * SHARD / 2.0);
            self.quad([
                Vec3::new(a.x, 0.0, a.y),
                Vec3::new(b.x, 0.0, b.y),
                Vec3::new(b.x, height, b.y),
                Vec3::new(a.x, height, a.y),
            ]);
        }
    }

    pub(super) fn build(self) -> Mesh {
        let count = self.positions.len();
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; count])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; count])
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

fn shape_mesh(shape: Shape) -> Mesh {
    let mut b = Builder::default();
    match shape {
        Shape::Cracks {
            inner,
            outer,
            lit,
            erupting,
            height,
        } => {
            let (start, end) = ((inner + 0.15).max(0.35), outer - 0.15);
            if end > start {
                for crack in 0..lit {
                    let angle = TAU * crack as f32 / CRACKS as f32;
                    let sign = if crack % 2 == 0 { 1.0 } else { -1.0 };
                    // Radius is exact at every joint, so a bend never leaves the annulus.
                    let points: [Vec2; 5] = std::array::from_fn(|k| {
                        let radius = start + (end - start) * k as f32 / 4.0;
                        let bend = [0.0, 0.10, -0.08, 0.10, 0.0][k] * sign;
                        polar(angle + bend / radius, radius)
                    });
                    for pair in points.windows(2) {
                        b.strip(pair[0], pair[1], LINE);
                    }
                }
            }
            if erupting {
                for shard in 0..12 {
                    let angle = TAU * (shard as f32 + 0.5) / 12.0;
                    b.shard(polar(angle, (inner + outer) / 2.0), shard_height(height));
                }
            }
        }
        Shape::Runes {
            radius,
            lit,
            tally,
            erupting,
            height,
        } => {
            b.circle(radius * 0.85, LINE);
            for glyph in 0..lit {
                let out = polar(TAU * glyph as f32 / GLYPHS as f32, 1.0);
                let side = out.perp();
                let centre = out * radius * 0.5;
                b.strip(
                    centre - out * radius * 0.2,
                    centre + out * radius * 0.2,
                    LINE,
                );
                // Two branches on one side of the stave: a rune, never a numeral.
                for k in [0.0, 0.1] {
                    let root = centre + out * radius * k;
                    b.strip(root, root + (out + side) * radius * 0.1, LINE);
                }
            }
            for bar in 0..tally {
                let x = (bar as f32 - (tally as f32 - 1.0) / 2.0) * radius * 0.07;
                b.strip(
                    Vec2::new(x, -radius * 0.1),
                    Vec2::new(x, radius * 0.1),
                    LINE,
                );
            }
            if erupting {
                for shard in 0..GLYPHS {
                    let angle = TAU * (shard as f32 + 0.5) / GLYPHS as f32;
                    b.shard(polar(angle, radius * 0.5), shard_height(height));
                }
            }
        }
        Shape::Notes {
            radius,
            notes,
            closing,
            erupting,
            height,
        } => {
            for note in 0..notes {
                b.circle(radius * (0.25 + 0.17 * f32::from(note)), LINE);
            }
            b.circle(radius * (0.92 - 0.70 * closing), LINE * 1.6);
            if erupting {
                for spoke in 0..8 {
                    let out = polar(TAU * spoke as f32 / 8.0, 1.0);
                    b.strip(out * radius * 0.1, out * radius * 0.9, LINE);
                }
                for shard in 0..6 {
                    let angle = TAU * (shard as f32 + 0.5) / 6.0;
                    b.shard(polar(angle, radius * 0.5), shard_height(height));
                }
            }
        }
        Shape::Spear { length } => return crystal(length, SPEAR_RADIUS),
    }
    b.build()
}

/// An elongated octahedron along +Z, centred on its own origin. Flat shaded.
pub(crate) fn crystal(length: f32, radius: f32) -> Mesh {
    let tips = [Vec3::Z * length / 2.0, -Vec3::Z * length / 2.0];
    let ring = [Vec3::X, Vec3::Y, -Vec3::X, -Vec3::Y].map(|v| v * radius);
    let mut positions = Vec::with_capacity(24);
    let mut normals = Vec::with_capacity(24);
    for (index, tip) in tips.into_iter().enumerate() {
        for i in 0..4 {
            let (mut a, mut b) = (ring[i], ring[(i + 1) % 4]);
            if index == 1 {
                std::mem::swap(&mut a, &mut b);
            }
            let normal = (b - a).cross(tip - a).normalize();
            positions.extend([a, b, tip].map(|v| v.to_array()));
            normals.extend([normal.to_array(); 3]);
        }
    }
    let count = positions.len() as u32;
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; count as usize])
    .with_inserted_indices(Indices::U32((0..count).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::MoveEnd;
    use EncounterMoveKind::*;
    use bevy::mesh::VertexAttributeValues;

    fn presented(
        kind: EncounterMoveKind,
        phase: MovePhase,
        volume: HazardVolume,
        ticks: u32,
        remaining: u32,
    ) -> PresentedMove {
        let mut announced = crate::player::encounters::tests::timeline().moves.remove(0);
        announced.kind = kind;
        announced.phase = phase;
        announced.phase_ticks = ticks;
        announced.hazards = vec![volume];
        announced.pulse = (phase == MovePhase::Channel).then_some((1, 3));
        PresentedMove {
            key: MoveKey {
                encounter: 5,
                boss: 9,
                instance: 11,
            },
            boss_kind: MobKind::DraugrKing,
            stage: 2,
            announced,
            window: Window::Current,
            progress: (ticks - remaining) as f32 / ticks as f32,
            remaining_ticks: remaining,
        }
    }

    fn open(_: IVec3) -> bool {
        false
    }

    fn ring(inner_radius: f32, radius: f32) -> HazardVolume {
        HazardVolume {
            shape: HazardShape::Ring { inner_radius },
            origin: [3.0, 65.0, -2.0],
            direction: [0.0; 3],
            radius,
            height: 2.0,
        }
    }

    fn disc(x: f32, radius: f32) -> HazardVolume {
        HazardVolume {
            shape: HazardShape::Disc,
            origin: [x, 65.2, 1.0],
            direction: [0.0; 3],
            radius,
            height: 2.4,
        }
    }

    fn lane() -> HazardVolume {
        HazardVolume {
            shape: HazardShape::Line { half_width: 0.9 },
            origin: [1.0, 65.4, 2.0],
            direction: [0.6, 0.0, -0.8],
            radius: 17.6,
            height: 2.6,
        }
    }

    fn points(shape: Shape, transform: Transform) -> Vec<Vec3> {
        let mesh = shape_mesh(shape);
        let Some(VertexAttributeValues::Float32x3(points)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        points
            .iter()
            .map(|&p| transform.transform_point(Vec3::from_array(p)))
            .collect()
    }

    #[test]
    fn every_spell_shape_stays_inside_its_announced_volume_on_every_tick() {
        for (kind, phase, volume, ticks) in [
            (Burial, MovePhase::Telegraph, ring(0.0, 2.0), 30),
            (Burial, MovePhase::Channel, ring(4.0, 6.0), 14),
            (EdictOfTheGraves, MovePhase::Telegraph, disc(7.0, 3.0), 30),
            (EdictOfTheGraves, MovePhase::Channel, disc(-7.0, 3.0), 16),
            (RequiemOfTheBuried, MovePhase::Channel, disc(6.0, 3.2), 18),
            (SepulchreSpear, MovePhase::Release, lane(), 16),
        ] {
            for remaining in 1..=ticks {
                let one = presented(kind, phase, volume, ticks, remaining);
                let (shape, transform) = effect_for(&one, &volume, &open)
                    .unwrap_or_else(|| panic!("{kind:?}/{phase:?} drew nothing"));
                assert!(shape_mesh(shape).count_vertices() <= MAX_VERTICES);
                let origin = Vec3::from_array(volume.origin);
                for p in points(shape, transform) {
                    assert!(p.is_finite());
                    assert!(
                        (origin.y - volume.height / 2.0..=origin.y + volume.height / 2.0)
                            .contains(&p.y),
                        "{kind:?} left its height: {p}"
                    );
                    let offset = (p - origin).xz();
                    let inside = match volume.shape {
                        HazardShape::Ring { inner_radius } => {
                            offset.length() >= inner_radius - 0.03
                                && offset.length() <= volume.radius + 0.03
                        }
                        HazardShape::Disc => offset.length() <= volume.radius + 0.03,
                        HazardShape::Line { half_width } => {
                            let direction = Vec3::from_array(volume.direction).xz();
                            let along = offset.dot(direction);
                            (-0.03..=volume.radius + 0.03).contains(&along)
                                && offset.perp_dot(direction).abs() <= half_width
                        }
                        HazardShape::Cone { .. } => false,
                    };
                    assert!(inside, "{kind:?}/{phase:?} left its region: {p}");
                }
            }
        }
    }

    #[test]
    fn lit_order_and_eruption_follow_server_ticks_without_a_local_clock() {
        let band = ring(4.0, 6.0);
        let mut last = 0;
        for remaining in (1..=14).rev() {
            let one = presented(Burial, MovePhase::Channel, band, 14, remaining);
            let (shape, _) = effect_for(&one, &band, &open).unwrap();
            let Shape::Cracks { lit, erupting, .. } = shape else {
                panic!("burial draws cracks");
            };
            assert!(lit >= last, "a crack went dark before contact");
            last = lit;
            assert_eq!(erupting, remaining == 1);
            let raised = points(shape, Transform::IDENTITY)
                .iter()
                .any(|p| p.y > 0.01);
            assert_eq!(raised, remaining == 1, "only the contact tick erupts");
            assert_eq!(
                effect_for(&one, &band, &open),
                effect_for(&one, &band, &open)
            );
        }
        assert_eq!(last, CRACKS);

        let sector = disc(7.0, 3.0);
        let edict = effect_for(
            &presented(EdictOfTheGraves, MovePhase::Channel, sector, 16, 8),
            &sector,
            &open,
        )
        .unwrap()
        .0;
        let requiem = effect_for(
            &presented(RequiemOfTheBuried, MovePhase::Channel, sector, 16, 8),
            &sector,
            &open,
        )
        .unwrap()
        .0;
        assert!(matches!(edict, Shape::Runes { tally: 2, .. }));
        assert!(matches!(requiem, Shape::Notes { notes: 2, .. }));
        assert_ne!(
            shape_mesh(edict).count_vertices(),
            shape_mesh(requiem).count_vertices()
        );

        let mut one = presented(EdictOfTheGraves, MovePhase::Channel, sector, 16, 8);
        for (window, ended) in [
            (Window::Upcoming, None),
            (Window::AwaitingUpdate, None),
            (Window::Current, Some(MoveEnd::Cancelled)),
        ] {
            (one.window, one.announced.ended) = (window, ended);
            assert_eq!(effect_for(&one, &sector, &open), None);
        }
        let mut recovery = presented(EdictOfTheGraves, MovePhase::Recovery, sector, 36, 8);
        assert_eq!(effect_for(&recovery, &sector, &open), None);
        recovery.announced.phase = MovePhase::Channel;
        recovery.boss_kind = MobKind::VargrGuardian;
        assert_eq!(effect_for(&recovery, &sector, &open), None);
    }

    #[test]
    fn the_spear_crosses_its_locked_lane_at_the_server_rate_and_stops_before_terrain() {
        let lane = lane();
        let direction = Vec3::from_array(lane.direction);
        for remaining in (1..=16).rev() {
            let one = presented(SepulchreSpear, MovePhase::Release, lane, 16, remaining);
            let crossed = (17 - remaining) as f32 * 17.6 / 16.0;
            assert!((flight(&one, &lane, &open) - crossed).abs() < 1e-3);
            let (shape, transform) = effect_for(&one, &lane, &open).unwrap();
            assert!((transform.rotation * Vec3::Z).abs_diff_eq(direction, 1e-4));
            let tip = points(shape, transform)
                .iter()
                .map(|p| (*p - Vec3::from_array(lane.origin)).dot(direction))
                .fold(f32::MIN, f32::max);
            assert!((tip - crossed).abs() < 1e-3, "tip {tip} vs {crossed}");
        }
        let mut straight = lane;
        straight.origin = [0.5, 65.4, 0.5];
        straight.direction = [0.0, 0.0, -1.0];
        let one = presented(SepulchreSpear, MovePhase::Release, straight, 16, 1);
        let wall = |voxel: IVec3| voxel.z <= -5;
        let stopped = flight(&one, &straight, &wall);
        assert!(stopped > 4.25 && stopped <= 4.5, "stopped at {stopped}");
        for phase in [MovePhase::Telegraph, MovePhase::Recovery] {
            let one = presented(SepulchreSpear, phase, lane, 16, 8);
            assert_eq!(effect_for(&one, &lane, &open), None, "{phase:?}");
        }
    }

    #[test]
    fn effects_follow_replacement_cancellation_and_release_every_mesh() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_resource::<EncounterPresentation>();
        register(&mut app);
        let count = |app: &mut App, shown: Vec<PresentedMove>| {
            app.world_mut().resource_mut::<EncounterPresentation>().0 = shown;
            app.update();
            let world = app.world_mut();
            let keys: Vec<_> = world
                .query::<&SpellEffect>()
                .iter(world)
                .map(|effect| effect.key)
                .collect();
            (keys, world.resource::<Assets<Mesh>>().len())
        };
        let mut edict = presented(EdictOfTheGraves, MovePhase::Channel, disc(7.0, 3.0), 16, 8);
        edict.announced.hazards.push(disc(-7.0, 3.0));
        assert_eq!(count(&mut app, vec![edict.clone()]).0.len(), 2);
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 2);

        let mut requiem = presented(
            RequiemOfTheBuried,
            MovePhase::Channel,
            disc(6.0, 3.2),
            18,
            1,
        );
        requiem.key.instance += 1;
        let (keys, meshes) = count(&mut app, vec![requiem.clone()]);
        assert_eq!((keys, meshes), (vec![requiem.key], 1));

        let mut physical = presented(KingsSentence, MovePhase::Release, lane(), 5, 2);
        physical.key.instance += 2;
        let mut vargr = requiem.clone();
        vargr.boss_kind = MobKind::VargrGuardian;
        assert_eq!(count(&mut app, vec![physical, vargr]), (vec![], 0));

        let mut stale = requiem;
        stale.window = Window::AwaitingUpdate;
        assert_eq!(count(&mut app, vec![stale]), (vec![], 0));
        assert_eq!(count(&mut app, vec![edict]).1, 2);
        assert_eq!(count(&mut app, vec![]), (vec![], 0));
        assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 3);
    }
}
