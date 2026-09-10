//! Final-stage regalia. The announced encounter stage (never health, a move name or a
//! local timer) drops the funeral mask, slips the welded crown and lights the core in
//! the chest fissure. The snapshot keeps the root and the body keeps its box; a body
//! first seen already in the final stage shows the result without replaying the fall.
use super::choreography::smooth;
use super::*;
use crate::net::{EncounterMoveKind, MovePhase};
use crate::player::encounters::{EncounterPresentation, Window, is_spell};

/// The encounter stage the approved design calls final, counted from one as the wire is.
pub(super) const FINAL_STAGE: u8 = 3;
const FALL_SECONDS: f32 = 0.6;
const CROWN_SECONDS: f32 = 0.8;
/// The mask plate's authored centre in the rest mesh.
const MASK_CENTRE: Vec3 = Vec3::new(0.10, 2.44, -0.19);
/// Where a dropped mask rests in the body frame of the moment it fell: face-up ahead of
/// the free-hand boot, clear of a planted blade, and still inside the body box.
const MASK_REST: Vec3 = Vec3::new(-0.30, 0.02, -0.40);
/// Flush with the chest fissure, in front of the recessed ice. The halves leave ±0.035
/// and end at z -0.235; the light stops just short of that face.
const CORE_CENTRE: Vec3 = Vec3::new(0.0, 1.79, -0.222);
/// Beyond the free hand's fingertips in the rest mesh, so a raised forearm holds it up.
const CRYSTAL_CENTRE: Vec3 = Vec3::new(-0.39, 1.10, -0.05);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mask {
    Worn,
    /// Seen crossing into the final stage; the next sample records where it starts.
    Detach,
    Falling {
        from: Mat4,
        to: Mat4,
        elapsed: f32,
    },
    /// World space, so the king walking away does not drag it along the floor.
    Fallen(Mat4),
    /// First seen already in the final stage: no fall was witnessed, none is invented.
    Absent,
}

#[derive(Debug, Clone, PartialEq)]
pub(in super::super) struct Regalia {
    stage: Option<u8>,
    crown: f32,
    mask: Mask,
    /// The mask transform relative to the root while it is off the face.
    local: Option<Transform>,
}

impl Default for Regalia {
    fn default() -> Self {
        Self {
            stage: None,
            crown: 0.0,
            mask: Mask::Worn,
            local: None,
        }
    }
}

impl Regalia {
    pub(super) fn crown(&self) -> f32 {
        smooth(self.crown)
    }

    pub(in super::super) fn final_stage(&self) -> bool {
        self.stage.is_some_and(|stage| stage >= FINAL_STAGE)
    }

    pub(in super::super) fn mask_visible(&self) -> bool {
        self.mask != Mask::Absent
    }

    /// `stage` is the live timeline's ordinal for this boss and `alive` the snapshot's
    /// statement. A corpse's evicted timeline keeps what it last showed; a living boss
    /// with no encounter is not in a fight, so a wipe's reset restores the regalia.
    pub(in super::super) fn observe(&mut self, stage: Option<u8>, alive: bool) {
        match stage {
            Some(stage) => {
                let seen = self.stage.replace(stage);
                if stage < FINAL_STAGE {
                    *self = Self {
                        stage: Some(stage),
                        ..default()
                    };
                } else if seen.is_none() && self.mask == Mask::Worn {
                    self.crown = 1.0;
                    self.mask = Mask::Absent;
                } else if seen.is_some_and(|seen| seen < FINAL_STAGE) && self.mask == Mask::Worn {
                    self.mask = Mask::Detach;
                }
            }
            None if alive => *self = default(),
            None => {}
        }
    }

    /// Advances cosmetic time and places a mask that has left the face.
    pub(super) fn update(&mut self, pose: &mut [Transform; 17], position: Vec3, yaw: f32, dt: f32) {
        if self.final_stage() {
            self.crown = (self.crown + dt / CROWN_SECONDS).min(1.0);
        }
        let root = Mat4::from_rotation_translation(Quat::from_rotation_y(yaw), position);
        let slot = Segment::Mask as usize;
        if self.mask == Mask::Detach {
            self.mask = Mask::Falling {
                from: root * pose[slot].to_matrix(),
                to: root * rest_matrix(),
                elapsed: 0.0,
            };
        }
        let world = match self.mask {
            Mask::Falling { from, to, elapsed } => {
                let elapsed = elapsed + dt;
                let t = (elapsed / FALL_SECONDS).min(1.0);
                self.mask = if t >= 1.0 {
                    Mask::Fallen(to)
                } else {
                    Mask::Falling { from, to, elapsed }
                };
                fall(from, to, t)
            }
            Mask::Fallen(world) => world,
            Mask::Worn | Mask::Detach | Mask::Absent => {
                self.local = None;
                return;
            }
        };
        let local = Transform::from_matrix(root.inverse() * world);
        pose[slot] = local;
        self.local = Some(local);
    }

    /// Applies the placed mask to a pose sampled after [`Self::update`].
    pub(super) fn dress(&self, pose: &mut [Transform; 17]) {
        if let Some(local) = self.local {
            pose[Segment::Mask as usize] = local;
        }
    }
}

fn rest_matrix() -> Mat4 {
    // Front (-Z) faces up, the brow points along +X and the thin plate lies on the floor.
    Mat4::from_translation(MASK_REST)
        * Mat4::from_mat3(Mat3::from_cols(Vec3::NEG_Z, Vec3::X, Vec3::NEG_Y))
        * Mat4::from_translation(-MASK_CENTRE)
}

fn fall(from: Mat4, to: Mat4, t: f32) -> Mat4 {
    let (_, start_rotation, _) = from.to_scale_rotation_translation();
    let (_, end_rotation, _) = to.to_scale_rotation_translation();
    let start = from.transform_point3(MASK_CENTRE);
    let end = to.transform_point3(MASK_CENTRE);
    let mut centre = start.lerp(end, smooth(t));
    // Accelerates down and turns face-up on the way; no bounce and no drift once down.
    centre.y = start.y + (end.y - start.y) * t * t;
    Mat4::from_rotation_translation(start_rotation.slerp(end_rotation, smooth(t)), centre)
        * Mat4::from_translation(-MASK_CENTRE)
}

/// The crown slips down over the bare-bone brow, hinged on the helm's right rear rim.
/// Every point moves down or inward, so the silhouette never grows.
pub(super) fn crown_tilt(amount: f32) -> Mat4 {
    around(
        Vec3::new(0.215, 2.6375, 0.215),
        Quat::from_rotation_z(0.18 * amount) * Quat::from_rotation_x(-0.08 * amount),
    )
}

#[derive(Resource)]
pub(in crate::player) struct RegaliaVisuals {
    core: Handle<Mesh>,
    lit: Handle<StandardMaterial>,
    flare: Handle<StandardMaterial>,
    crystal: Handle<Mesh>,
    crystal_material: Handle<StandardMaterial>,
}

/// The fissure light. Not a rig segment and not flashed: it is presentation of state.
#[derive(Component)]
pub(in crate::player) struct CoreGlow {
    owner: Entity,
}

/// The Sepulchre Spear crystal forming in the free hand during its announced telegraph.
#[derive(Component)]
pub(in crate::player) struct HandCrystal {
    owner: Entity,
}

pub(in crate::player) fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut glow = |colour| {
        materials.add(StandardMaterial {
            base_color: colour,
            unlit: true,
            fog_enabled: false,
            cull_mode: None,
            ..default()
        })
    };
    commands.insert_resource(RegaliaVisuals {
        lit: glow(Color::srgb(0.55, 0.88, 1.0)),
        flare: glow(Color::srgb(0.92, 0.99, 1.0)),
        crystal_material: glow(Color::srgb(0.62, 0.90, 1.0)),
        core: meshes.add(Cuboid::new(0.05, 0.84, 0.02)),
        crystal: meshes.add(crate::player::encounters::spells::crystal(0.34, 0.09)),
    });
}

/// Dresses each king once, then presents mask, core and crystal from server state.
#[allow(clippy::type_complexity)] // Disjoint child queries keep every mutable access provable.
pub(in crate::player) fn present(
    mut commands: Commands,
    visuals: Option<Res<RegaliaVisuals>>,
    presentation: Res<EncounterPresentation>,
    mobs: Query<&Mob>,
    mut parts: Query<
        (Entity, &MobVisual, Option<&mut Visibility>),
        (Without<CoreGlow>, Without<HandCrystal>),
    >,
    mut cores: Query<
        (
            &CoreGlow,
            &mut Visibility,
            &mut MeshMaterial3d<StandardMaterial>,
        ),
        (Without<MobVisual>, Without<HandCrystal>),
    >,
    mut crystals: Query<
        (&HandCrystal, &mut Visibility, &mut Transform),
        (Without<MobVisual>, Without<CoreGlow>),
    >,
) {
    let Some(visuals) = visuals else {
        return;
    };
    let dressed: Vec<Entity> = cores.iter().map(|(core, ..)| core.owner).collect();
    for (entity, part, visibility) in &mut parts {
        let Some(regalia) = mobs
            .get(part.owner)
            .ok()
            .and_then(|mob| mob.king_motion.as_ref())
            .map(|motion| &motion.regalia)
        else {
            continue;
        };
        match part.part {
            MobPart::King(Segment::Mask) => {
                let wanted = if regalia.mask_visible() {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                // A body spawned without a renderer carries no visibility until needed.
                match visibility {
                    Some(mut visibility) => {
                        visibility.set_if_neq(wanted);
                    }
                    None if wanted == Visibility::Hidden => {
                        commands.entity(entity).insert(wanted);
                    }
                    None => {}
                }
            }
            MobPart::King(Segment::Torso) if !dressed.contains(&part.owner) => {
                commands.spawn((
                    CoreGlow { owner: part.owner },
                    Mesh3d(visuals.core.clone()),
                    MeshMaterial3d(visuals.lit.clone()),
                    Transform::from_translation(CORE_CENTRE),
                    Visibility::Hidden,
                    ChildOf(entity),
                ));
            }
            MobPart::King(Segment::ForeLeft) if !dressed.contains(&part.owner) => {
                commands.spawn((
                    HandCrystal { owner: part.owner },
                    Mesh3d(visuals.crystal.clone()),
                    MeshMaterial3d(visuals.crystal_material.clone()),
                    Transform::from_translation(CRYSTAL_CENTRE)
                        .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
                    Visibility::Hidden,
                    ChildOf(entity),
                ));
            }
            _ => {}
        }
    }
    let current = |boss: u64| {
        presentation.0.iter().filter(move |one| {
            one.key.boss == boss && one.window == Window::Current && one.announced.ended.is_none()
        })
    };
    for (core, mut visibility, mut material) in &mut cores {
        let Ok(mob) = mobs.get(core.owner) else {
            continue;
        };
        let alive = mob.falling.is_none();
        let lit = alive
            && mob
                .king_motion
                .as_ref()
                .is_some_and(|motion| motion.regalia.final_stage());
        // A spell's authoritative release or pulse contact, and nothing locally counted.
        let pulse = alive
            && current(mob.entity_id).any(|one| is_spell(one.announced.kind) && one.damaging());
        visibility.set_if_neq(if lit || pulse {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
        let next = if pulse { &visuals.flare } else { &visuals.lit };
        if material.0 != *next {
            material.0 = next.clone();
        }
    }
    for (crystal, mut visibility, mut transform) in &mut crystals {
        let forming = mobs.get(crystal.owner).ok().and_then(|mob| {
            mob.falling.is_none().then_some(())?;
            current(mob.entity_id).find(|one| {
                one.announced.kind == EncounterMoveKind::SepulchreSpear
                    && one.announced.phase == MovePhase::Telegraph
            })
        });
        match forming {
            Some(one) => {
                visibility.set_if_neq(Visibility::Inherited);
                let scale = Vec3::splat(0.25 + 0.55 * smooth(one.progress));
                if transform.scale != scale {
                    transform.scale = scale;
                }
            }
            None => {
                visibility.set_if_neq(Visibility::Hidden);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::choreography::{Controls, assemble};
    use super::super::motion::REST_FEET;
    use super::*;
    use bevy::mesh::VertexAttributeValues;

    fn vertices(segment: Segment, matrix: Mat4) -> Vec<Vec3> {
        let mesh = geometry(segment);
        let Some(VertexAttributeValues::Float32x3(points)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        points
            .iter()
            .map(|&point| matrix.transform_point3(Vec3::from_array(point)))
            .collect()
    }

    fn sampled(regalia: &mut Regalia, position: Vec3, yaw: f32, dt: f32) -> [Transform; 17] {
        let mut pose = assemble(
            Controls {
                crown: regalia.crown(),
                ..default()
            },
            REST_FEET,
        );
        regalia.update(&mut pose, position, yaw, dt);
        pose
    }

    #[test]
    fn final_stage_drops_the_mask_to_the_floor_and_tilts_the_crown_inside_the_body_box() {
        let envelope = body(MobKind::DraugrKing);
        let (position, yaw) = (Vec3::new(5.0, 64.0, 2.0), 0.7);
        let root = Mat4::from_rotation_translation(Quat::from_rotation_y(yaw), position);
        let mut regalia = Regalia::default();
        regalia.observe(Some(2), true);
        let worn = sampled(&mut regalia, position, yaw, 0.1);
        assert_eq!(worn[Segment::Mask as usize], worn[Segment::Head as usize]);
        assert_eq!(regalia.crown(), 0.0);

        regalia.observe(Some(3), true);
        let mut last = worn;
        for _ in 0..60 {
            last = sampled(&mut regalia, position, yaw, 1.0 / 60.0);
            let lowest = vertices(
                Segment::Mask,
                root * last[Segment::Mask as usize].to_matrix(),
            )
            .iter()
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);
            assert!(
                lowest >= position.y - 0.005,
                "mask passed the floor: {lowest}"
            );
            assert_eq!(
                last[Segment::Pelvis as usize],
                worn[Segment::Pelvis as usize]
            );
        }
        assert!(matches!(regalia.mask, Mask::Fallen(_)));
        let landed = vertices(
            Segment::Mask,
            root * last[Segment::Mask as usize].to_matrix(),
        );
        for point in &landed {
            let local = root.inverse().transform_point3(*point);
            assert!(
                (-0.003..0.043).contains(&local.y),
                "not lying flat: {local}"
            );
            assert!(local.x.abs() <= envelope.width / 2.0 && local.z.abs() <= envelope.width / 2.0);
        }
        // Once down, it belongs to the floor rather than the moving king.
        let (moved, turned) = (Vec3::new(9.0, 64.0, -1.0), 1.9);
        let later = sampled(&mut regalia, moved, turned, 0.1);
        let root = Mat4::from_rotation_translation(Quat::from_rotation_y(turned), moved);
        for (a, b) in landed.iter().zip(vertices(
            Segment::Mask,
            root * later[Segment::Mask as usize].to_matrix(),
        )) {
            assert!(a.distance(b) < 1e-3);
        }

        assert_eq!(regalia.crown(), 1.0);
        let head = later[Segment::Head as usize].to_matrix();
        let rest = vertices(Segment::Crown, head);
        let tilted = vertices(Segment::Crown, later[Segment::Crown as usize].to_matrix());
        assert!(rest.iter().zip(&tilted).any(|(a, b)| a.distance(*b) > 0.08));
        let top = rest.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        for point in tilted {
            assert!(point.y <= top + 1e-4 && point.y <= envelope.height + 1e-4);
            assert!(point.x.abs() <= envelope.width / 2.0 && point.z.abs() <= envelope.width / 2.0);
        }
    }

    #[test]
    fn late_final_stage_shows_the_result_and_only_a_living_withdrawal_restores_it() {
        let mut late = Regalia::default();
        late.observe(Some(3), true);
        assert!(!late.mask_visible() && late.final_stage());
        assert_eq!(late.crown(), 1.0);
        let pose = sampled(&mut late, Vec3::ZERO, 0.0, 0.1);
        assert_eq!(pose[Segment::Mask as usize], pose[Segment::Head as usize]);
        // The inbox forgets a corpse's encounter; the corpse keeps what it showed.
        late.observe(None, false);
        assert!(!late.mask_visible() && late.final_stage());

        let mut fought = Regalia::default();
        fought.observe(Some(2), true);
        fought.observe(Some(3), true);
        sampled(&mut fought, Vec3::ZERO, 0.0, 1.0);
        assert!(fought.local.is_some());
        // A wipe ends the encounter on a living boss: the reset king wears it again.
        fought.observe(None, true);
        assert_eq!(fought, Regalia::default());
        let pose = sampled(&mut fought, Vec3::ZERO, 0.0, 0.1);
        assert_eq!(pose[Segment::Mask as usize], pose[Segment::Head as usize]);
        // A newer pull starting at stage one is also a whole mask, whatever came before.
        let mut replaced = Regalia::default();
        replaced.observe(Some(3), true);
        replaced.observe(Some(1), true);
        assert!(replaced.mask_visible() && !replaced.final_stage());
        assert_eq!(replaced.crown(), 0.0);
    }

    #[test]
    fn core_glow_and_hand_crystal_follow_the_announced_stage_and_spell() {
        use crate::net::{EncounterTimelineInbox, MobState};
        use crate::player::mobs::tests::{deliver, draugr, headless};
        use bevy::time::TimeUpdateStrategy;
        let mut app = headless();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            50,
        )));
        let mut state = MobState {
            kind: MobKind::DraugrKing,
            ..draugr(900, 3.0, 100, MobAction::Windup)
        };
        let push = |app: &mut App, stage: u8, kind, phase, started: u32, ticks: u32| {
            let mut timeline = crate::player::encounters::tests::timeline();
            timeline.boss = MobKind::DraugrKing;
            timeline.boss_entity_id = 900;
            timeline.phase = stage;
            let one = &mut timeline.moves[0];
            (one.kind, one.phase, one.phase_started_tick, one.phase_ticks) =
                (kind, phase, started, ticks);
            one.pulse = (phase == MovePhase::Channel).then_some((0, 3));
            app.world_mut()
                .resource_mut::<EncounterTimelineInbox>()
                .push(timeline);
        };
        let read = |app: &mut App| {
            let world = app.world_mut();
            let visuals = world.resource::<RegaliaVisuals>();
            let (lit, flare) = (visuals.lit.clone(), visuals.flare.clone());
            let (core_visible, material) = world
                .query::<(&CoreGlow, &Visibility, &MeshMaterial3d<StandardMaterial>)>()
                .single(world)
                .map(|(_, v, m)| (*v == Visibility::Inherited, m.0.clone()))
                .unwrap();
            let (crystal_visible, scale) = world
                .query::<(&HandCrystal, &Visibility, &Transform)>()
                .single(world)
                .map(|(_, v, t)| (*v == Visibility::Inherited, t.scale.x))
                .unwrap();
            let state = if !core_visible {
                "hidden"
            } else if material == flare {
                "flare"
            } else if material == lit {
                "lit"
            } else {
                "other"
            };
            (state, crystal_visible, scale)
        };

        push(
            &mut app,
            2,
            EncounterMoveKind::SepulchreSpear,
            MovePhase::Telegraph,
            100,
            20,
        );
        deliver(&mut app, 110, vec![state]);
        for _ in 0..3 {
            app.update();
        }
        let (core, crystal, scale) = read(&mut app);
        assert_eq!(
            core, "hidden",
            "stages before the last keep the core recessed"
        );
        assert!(crystal && (scale - (0.25 + 0.55 * smooth(0.5))).abs() < 1e-4);

        push(
            &mut app,
            3,
            EncounterMoveKind::RequiemOfTheBuried,
            MovePhase::Channel,
            110,
            18,
        );
        deliver(&mut app, 111, vec![state]);
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(read(&mut app).0, "lit");
        assert!(!read(&mut app).1, "a ritual forms no spear crystal");
        deliver(&mut app, 127, vec![state]);
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(read(&mut app).0, "flare", "the server's pulse contact tick");
        let world = app.world_mut();
        let masks: Vec<_> = world
            .query::<(&MobVisual, Option<&Visibility>)>()
            .iter(world)
            .filter(|(part, _)| part.part == MobPart::King(Segment::Mask))
            .map(|(_, visibility)| visibility.copied())
            .collect();
        assert_eq!(masks.len(), 1);
        assert_ne!(
            masks[0],
            Some(Visibility::Hidden),
            "a witnessed fall stays drawn"
        );

        state.action = MobAction::Corpse;
        deliver(&mut app, 128, vec![state]);
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(read(&mut app).0, "hidden", "a corpse holds no light");
    }
}
