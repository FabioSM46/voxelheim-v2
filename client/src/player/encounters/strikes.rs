//! Strike reach: a planted blow drawn from the striking body out to the far boundary the
//! server announced, only while that announced release window is current.
//!
//! The visible fang, claw and blade stop short of the regions the server resolves against:
//! 0.83–0.98 blocks from the Vargr's centre against 3.0–3.6, and 1.52–1.74 from the king's
//! against 3.3 (#1028, #1034, and #1037 part 2, which kept those regions as engagement
//! reach). This layer draws the part of each blow the model cannot, so a blow landing at
//! its boundary is seen to reach it. Everything comes from the announcement — kind, window,
//! ticks and volume. Nothing here extends, shrinks or re-aims what the server decided, and
//! like the spell layer it carries no information the boundary cues lack.

use bevy::prelude::*;

use super::cues::placement;
use super::spells::{Builder, polar};
use super::{EncounterPresentation, MoveKey, PresentedMove, Window, reconcile};
use crate::net::{EncounterMoveKind, HazardShape, HazardVolume, MobKind, MovePhase};

/// Width of one drawn stroke, in blocks. Every stroke stays at least half of it inside
/// the announced boundary.
const STROKE: f32 = 0.06;
/// Above the spell layer, so the three floor layers never share a plane.
const LIFT: f32 = 0.035;
const ARC_STEPS: usize = 16;
/// Per effect; one planted blow announces one region.
#[cfg(test)]
const MAX_VERTICES: usize = 768;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Style {
    /// Prisoner claws: three furrows raked out across the cone.
    Rake,
    /// Bite and tear, bonebreaker jaws: two jaw lines closing across the cone's far end.
    Bite,
    /// The tolls' cuts: the blade's line swept across the cone at its far boundary.
    Sweep,
    /// The sentence and the third toll: a cut along the strip to its far end.
    Cleave,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Strike {
    style: Style,
    /// The announced direction's bearing on the floor, as `atan2(z, x)`.
    bearing: f32,
    /// A cone's half angle or a strip's half width.
    spread: f32,
    /// Where the stroke starts: the striking body's edge.
    inner: f32,
    /// How far the stroke has reached on the presented tick.
    reach: f32,
    /// The announced boundary, less one stroke width.
    far: f32,
    /// The share of the release's ticks that have run, including the presented one.
    progress: f32,
}

#[derive(Component)]
struct StrikeEffect {
    key: MoveKey,
    index: usize,
    strike: Strike,
}

#[derive(Resource)]
struct StrikeMaterial(Handle<StandardMaterial>);

/// Test-only: the move instances whose strike strokes are drawn now.
#[cfg(test)]
pub(super) fn drawn(world: &mut World) -> std::collections::HashSet<MoveKey> {
    world
        .query::<(&StrikeEffect, &InheritedVisibility)>()
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
    commands.insert_resource(StrikeMaterial(materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.93, 0.80),
        unlit: true,
        fog_enabled: false,
        cull_mode: None,
        ..default()
    })));
}

#[allow(clippy::type_complexity)] // One query per effect, matching the spell layer.
fn refresh(
    mut commands: Commands,
    presentation: Res<EncounterPresentation>,
    material: Res<StrikeMaterial>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut effects: Query<(Entity, &mut StrikeEffect, &Mesh3d, &mut Transform)>,
) {
    if !presentation.is_changed() {
        return;
    }
    let mut wanted = Vec::new();
    for one in &presentation.0 {
        for (index, volume) in one.hazards().iter().enumerate() {
            if let Some((strike, transform)) = strike_for(one, volume) {
                wanted.push((one.key, index, strike, transform));
            }
        }
    }
    let mut held = Vec::with_capacity(wanted.len());
    for (entity, mut effect, mesh, mut placed) in &mut effects {
        let Some(&(_, _, strike, transform)) = wanted
            .iter()
            .find(|(key, index, ..)| *key == effect.key && *index == effect.index)
        else {
            meshes.remove(mesh.0.id());
            commands.entity(entity).despawn();
            continue;
        };
        if effect.strike != strike {
            if let Some(mut asset) = meshes.get_mut(&mesh.0) {
                *asset = strike_mesh(strike);
            }
            effect.strike = strike;
        }
        *placed = transform;
        held.push((effect.key, effect.index));
    }
    for (key, index, strike, transform) in wanted {
        if held.contains(&(key, index)) {
            continue;
        }
        commands.spawn((
            StrikeEffect { key, index, strike },
            Mesh3d(meshes.add(strike_mesh(strike))),
            MeshMaterial3d(material.0.clone()),
            transform,
        ));
    }
}

/// The strike a presented move draws in one of its announced regions, or none.
///
/// Only a planted blow in a current release draws one: the telegraph already shows the
/// region, the recovery endangers nothing, and an ended, stale or upcoming window is not the
/// release the server is resolving.
fn strike_for(one: &PresentedMove, volume: &HazardVolume) -> Option<(Strike, Transform)> {
    use EncounterMoveKind::*;
    if one.window != Window::Current
        || one.announced.ended.is_some()
        || one.announced.phase != MovePhase::Release
    {
        return None;
    }
    let (style, spread) = match (one.boss_kind, one.announced.kind, volume.shape) {
        (MobKind::VargrGuardian, PrisonerClaws, HazardShape::Cone { half_angle }) => {
            (Style::Rake, half_angle)
        }
        (
            MobKind::VargrGuardian,
            BiteAndTear | BonebreakerJaws,
            HazardShape::Cone { half_angle },
        ) => (Style::Bite, half_angle),
        (MobKind::DraugrKing, ThreeTolls, HazardShape::Cone { half_angle }) => {
            (Style::Sweep, half_angle)
        }
        (MobKind::DraugrKing, KingsSentence | ThreeTolls, HazardShape::Line { half_width }) => {
            (Style::Cleave, half_width)
        }
        _ => return None,
    };
    let forward = Vec2::new(volume.direction[0], volume.direction[2]);
    if forward.length_squared() < 1e-6 || volume.radius <= STROKE * 2.0 {
        return None;
    }
    let far = volume.radius - STROKE;
    let inner = (super::super::mobs::body(one.boss_kind).width / 2.0).min(far / 2.0);
    let ticks = one.announced.phase_ticks.max(1);
    let progress = (ticks.saturating_sub(one.remaining_ticks) + 1).min(ticks) as f32 / ticks as f32;
    let mut transform = placement(*volume, one.boss_kind);
    transform.translation.y += LIFT;
    Some((
        Strike {
            style,
            bearing: forward.y.atan2(forward.x),
            spread,
            inner,
            reach: inner + (far - inner) * progress,
            far,
            progress,
        },
        transform,
    ))
}

fn arc(b: &mut Builder, from: f32, to: f32, radius: f32) {
    if (to - from).abs() < 1e-4 {
        return;
    }
    for step in 0..ARC_STEPS {
        let a = from + (to - from) * step as f32 / ARC_STEPS as f32;
        let c = from + (to - from) * (step + 1) as f32 / ARC_STEPS as f32;
        b.strip(polar(a, radius), polar(c, radius), STROKE);
    }
}

fn strike_mesh(strike: Strike) -> Mesh {
    let Strike {
        style,
        bearing,
        spread,
        inner,
        reach,
        far,
        progress,
    } = strike;
    let mut b = Builder::default();
    let radial = |b: &mut Builder, angle: f32, to: f32| {
        b.strip(polar(angle, inner), polar(angle, to), STROKE);
    };
    match style {
        Style::Rake => {
            for share in [-0.5, 0.0, 0.5] {
                radial(&mut b, bearing + spread * share, reach);
            }
        }
        Style::Bite => {
            let side = spread * 0.6;
            radial(&mut b, bearing - side, reach);
            radial(&mut b, bearing + side, reach);
            arc(&mut b, bearing - side, bearing + side, reach);
        }
        Style::Sweep => {
            let side = spread * 0.8;
            let blade = bearing - side + 2.0 * side * progress;
            radial(&mut b, blade, far);
            arc(&mut b, bearing - side, blade, far);
        }
        Style::Cleave => {
            let forward = polar(bearing, 1.0);
            let bar = forward.perp() * spread * 0.6;
            b.strip(forward * inner, forward * reach, STROKE);
            b.strip(forward * reach - bar, forward * reach + bar, STROKE);
        }
    }
    b.build()
}

#[cfg(test)]
mod capture;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::MoveEnd;
    use EncounterMoveKind::*;
    use bevy::mesh::VertexAttributeValues;

    fn presented(
        boss: MobKind,
        kind: EncounterMoveKind,
        phase: MovePhase,
        volume: HazardVolume,
        ticks: u32,
        remaining: u32,
    ) -> PresentedMove {
        let mut announced = crate::player::encounters::tests::timeline().moves.remove(0);
        (announced.kind, announced.phase, announced.phase_ticks) = (kind, phase, ticks);
        announced.hazards = vec![volume];
        PresentedMove {
            key: MoveKey {
                encounter: 5,
                boss: 9,
                instance: 11,
            },
            boss_kind: boss,
            stage: 1,
            announced,
            window: Window::Current,
            progress: (ticks - remaining) as f32 / ticks as f32,
            remaining_ticks: remaining,
        }
    }

    fn cone(half_angle: f32, radius: f32, bearing: f32, y: f32, height: f32) -> HazardVolume {
        HazardVolume {
            shape: HazardShape::Cone { half_angle },
            origin: [2.0, y, -1.0],
            direction: [bearing.cos(), 0.0, bearing.sin()],
            radius,
            height,
        }
    }

    fn line(half_width: f32, radius: f32) -> HazardVolume {
        HazardVolume {
            shape: HazardShape::Line { half_width },
            origin: [2.0, 65.4, -1.0],
            direction: [0.6, 0.0, -0.8],
            radius,
            height: 3.0,
        }
    }

    fn points(strike: Strike, transform: Transform) -> Vec<Vec3> {
        let mesh = strike_mesh(strike);
        assert!(mesh.count_vertices() <= MAX_VERTICES);
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

    /// How far a point lies into the region along its reach: radius for a cone, distance
    /// along the strip for a line. `None` when the point is outside the region.
    fn depth(volume: &HazardVolume, p: Vec3) -> Option<f32> {
        let origin = Vec3::from_array(volume.origin);
        if !(origin.y - volume.height / 2.0..=origin.y + volume.height / 2.0).contains(&p.y) {
            return None;
        }
        let offset = (p - origin).xz();
        let direction = Vec3::from_array(volume.direction).xz().normalize();
        match volume.shape {
            HazardShape::Cone { half_angle } => {
                let distance = offset.length();
                let angle = direction.angle_to(offset).abs();
                (distance <= volume.radius + 1e-4 && angle <= half_angle + 1e-4).then_some(distance)
            }
            HazardShape::Line { half_width } => {
                let along = offset.dot(direction);
                ((-1e-4..=volume.radius + 1e-4).contains(&along)
                    && offset.perp_dot(direction).abs() <= half_width + 1e-4)
                    .then_some(along)
            }
            _ => None,
        }
    }

    const PLANTED: [(MobKind, EncounterMoveKind, u32); 6] = [
        (MobKind::VargrGuardian, BiteAndTear, 4),
        (MobKind::VargrGuardian, PrisonerClaws, 6),
        (MobKind::VargrGuardian, BonebreakerJaws, 6),
        (MobKind::DraugrKing, KingsSentence, 5),
        (MobKind::DraugrKing, ThreeTolls, 4),
        (MobKind::DraugrKing, ThreeTolls, 4),
    ];

    fn region(index: usize) -> HazardVolume {
        [
            cone(0.70, 3.0, 0.3, 64.9, 2.2),
            cone(1.05, 3.4, -1.2, 64.9, 2.2),
            cone(0.38, 3.6, 2.0, 64.9, 2.2),
            line(1.1, 3.3),
            cone(0.95, 3.3, 0.45, 65.4, 3.0),
            line(0.65, 3.3),
        ][index]
    }

    #[test]
    fn every_strike_stays_inside_its_announced_volume_and_reaches_its_boundary_on_the_last_release_tick()
     {
        for (index, (boss, kind, ticks)) in PLANTED.into_iter().enumerate() {
            let volume = region(index);
            let mut deepest = 0.0_f32;
            for remaining in (1..=ticks).rev() {
                let one = presented(boss, kind, MovePhase::Release, volume, ticks, remaining);
                let (strike, transform) = strike_for(&one, &volume)
                    .unwrap_or_else(|| panic!("{kind:?} drew nothing on release"));
                let reached = points(strike, transform)
                    .into_iter()
                    .map(|p| {
                        assert!(p.is_finite());
                        depth(&volume, p).unwrap_or_else(|| {
                            panic!(
                                "{kind:?} left its announced region at {p}, {remaining} remaining"
                            )
                        })
                    })
                    .fold(0.0, f32::max);
                assert!(
                    reached + 1e-4 >= deepest,
                    "{kind:?} withdrew from {deepest} to {reached}"
                );
                deepest = reached;
            }
            assert!(
                deepest >= volume.radius - 0.1,
                "{kind:?} reached {deepest} of its {} boundary on the last release tick",
                volume.radius
            );
            let first = presented(boss, kind, MovePhase::Release, volume, ticks, ticks);
            let (strike, _) = strike_for(&first, &volume).unwrap();
            if strike.style != Style::Sweep {
                assert!(
                    strike.reach < volume.radius - 0.5,
                    "{kind:?} reached its boundary before the release had run"
                );
            }
        }
    }

    #[test]
    fn the_toll_blade_crosses_its_cone_with_the_release_ticks() {
        let volume = region(4);
        let mut last = f32::MIN;
        for remaining in (1..=4).rev() {
            let one = presented(
                MobKind::DraugrKing,
                ThreeTolls,
                MovePhase::Release,
                volume,
                4,
                remaining,
            );
            let (strike, _) = strike_for(&one, &volume).unwrap();
            let blade =
                strike.bearing - strike.spread * 0.8 + 1.6 * strike.spread * strike.progress;
            assert!(blade > last, "the blade turned back");
            last = blade;
            assert_eq!(strike_for(&one, &volume), strike_for(&one, &volume));
        }
        assert!((last - (0.45 + 0.95 * 0.8)).abs() < 1e-4);
    }

    #[test]
    fn nothing_is_drawn_outside_a_current_release_or_for_moves_that_are_not_planted_blows() {
        let bite = region(0);
        for phase in [MovePhase::Telegraph, MovePhase::Recovery] {
            let one = presented(MobKind::VargrGuardian, BiteAndTear, phase, bite, 4, 2);
            assert_eq!(strike_for(&one, &bite), None, "{phase:?}");
        }
        let mut one = presented(
            MobKind::VargrGuardian,
            BiteAndTear,
            MovePhase::Release,
            bite,
            4,
            2,
        );
        for (window, ended) in [
            (Window::Upcoming, None),
            (Window::AwaitingUpdate, None),
            (Window::Current, Some(MoveEnd::Cancelled)),
        ] {
            (one.window, one.announced.ended) = (window, ended);
            assert_eq!(strike_for(&one, &bite), None, "{window:?} {ended:?}");
        }
        let lane = HazardVolume {
            radius: 9.9,
            ..line(1.4, 9.9)
        };
        let disc = HazardVolume {
            shape: HazardShape::Disc,
            ..bite
        };
        for (boss, kind, volume) in [
            (MobKind::VargrGuardian, CollarCharge, lane),
            (MobKind::VargrGuardian, PredatorLeap, disc),
            (MobKind::DraugrKing, SepulchreSpear, lane),
            (MobKind::DraugrKing, BiteAndTear, bite),
            (MobKind::VargrGuardian, KingsSentence, line(1.1, 3.3)),
        ] {
            let one = presented(boss, kind, MovePhase::Release, volume, 6, 3);
            assert_eq!(strike_for(&one, &volume), None, "{boss:?} {kind:?}");
        }
    }

    #[test]
    fn strikes_follow_replacement_and_cancellation_and_release_every_mesh() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_resource::<EncounterPresentation>();
        register(&mut app);
        let show = |app: &mut App, shown: Vec<PresentedMove>| {
            app.world_mut().resource_mut::<EncounterPresentation>().0 = shown;
            app.update();
            let world = app.world_mut();
            let keys: Vec<_> = world
                .query::<&StrikeEffect>()
                .iter(world)
                .map(|effect| effect.key)
                .collect();
            (keys, world.resource::<Assets<Mesh>>().len())
        };
        let bite = region(0);
        let first = presented(
            MobKind::VargrGuardian,
            BiteAndTear,
            MovePhase::Release,
            bite,
            4,
            3,
        );
        assert_eq!(show(&mut app, vec![first.clone()]), (vec![first.key], 1));
        let mut later = first.clone();
        later.remaining_ticks = 1;
        assert_eq!(show(&mut app, vec![later]), (vec![first.key], 1));
        let mut replacement = first.clone();
        replacement.key.instance += 1;
        assert_eq!(
            show(&mut app, vec![replacement.clone()]),
            (vec![replacement.key], 1)
        );
        let mut cancelled = replacement;
        cancelled.announced.ended = Some(MoveEnd::Cancelled);
        assert_eq!(show(&mut app, vec![cancelled]), (vec![], 0));
        assert_eq!(show(&mut app, vec![first]).1, 1);
        assert_eq!(show(&mut app, vec![]), (vec![], 0));
        assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 1);
    }
}
