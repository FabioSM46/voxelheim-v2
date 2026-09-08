//! Boss move readings projected above the authoritative body. Geometry and words
//! carry the distinction even with silent audio and no cosmetic effects.

use std::time::{Duration, Instant};

use bevy::prelude::*;

use crate::net::{EncounterMoveKind, MobKind, MovePhase, Session};
use crate::player::encounters::{EncounterPresentation, MoveKey, PresentedMove, Window, is_spell};
use crate::player::{SnapshotBuffer, WorldCamera};

const WIDTH: f32 = 380.0;
const HEIGHT: f32 = 88.0;

#[derive(Component)]
struct MoveReading(MoveKey);
#[derive(Component)]
struct MoveLabel(MoveKey);
#[derive(Component)]
struct MoveFill(MoveKey);

pub(crate) struct EncounterUiPlugin;

impl Plugin for EncounterUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EncounterPresentation>()
            .add_systems(PostUpdate, refresh.before(bevy::ui::UiSystems::Layout));
    }
}

fn move_name(kind: EncounterMoveKind) -> &'static str {
    match kind {
        EncounterMoveKind::BiteAndTear => "Bite and tear",
        EncounterMoveKind::CollarCharge => "Collar charge",
        EncounterMoveKind::PredatorLeap => "Predator leap",
        EncounterMoveKind::PrisonerClaws => "Prisoner claws",
        EncounterMoveKind::BonebreakerJaws => "Bonebreaker jaws",
        EncounterMoveKind::KingsSentence => "King's sentence",
        EncounterMoveKind::ThreeTolls => "Three tolls",
        EncounterMoveKind::Burial => "Burial",
        EncounterMoveKind::EdictOfTheGraves => "Edict of the graves",
        EncounterMoveKind::SepulchreSpear => "Sepulchre spear",
        EncounterMoveKind::RequiemOfTheBuried => "Requiem of the buried",
    }
}

fn reading(one: &PresentedMove) -> String {
    let boss = match one.boss_kind {
        MobKind::VargrGuardian => "Vargr",
        MobKind::DraugrKing => "Draugr king",
        _ => "Boss",
    };
    let phase = match (one.window, one.announced.phase) {
        (Window::AwaitingUpdate, _) => "Awaiting server update",
        (Window::Upcoming, _) => "Announced",
        (_, MovePhase::Telegraph) if is_spell(one.announced.kind) => "Casting",
        (_, MovePhase::Telegraph) => "Preparing",
        (_, MovePhase::Release) => "ACTIVE",
        (_, MovePhase::Channel) if one.damaging() => "PULSE ACTIVE",
        (_, MovePhase::Channel) => "Channeling",
        (_, MovePhase::Recovery) => "Recovering",
    };
    let progress = (one.progress * 100.0).round() as u32;
    let detail = if let Some((index, total)) = one.announced.pulse {
        format!(
            " - pulse {}/{}{}",
            u16::from(index) + 1,
            total,
            if one.announced.interruptible {
                " [interruptible]"
            } else {
                ""
            }
        )
    } else {
        String::new()
    };
    format!(
        "{boss} - stage {}\n{}\n{phase} {progress}%{detail}",
        one.stage,
        move_name(one.announced.kind)
    )
}

#[allow(clippy::too_many_arguments)] // Independent UI queries remain disjoint and read-only inputs explicit.
fn refresh(
    mut commands: Commands,
    presentation: Res<EncounterPresentation>,
    session: Option<Res<Session>>,
    snapshots: Option<Res<SnapshotBuffer>>,
    cameras: Query<(&Camera, &Transform), With<WorldCamera>>,
    mut roots: Query<(Entity, &MoveReading, &mut Node)>,
    mut labels: Query<(&MoveLabel, &mut Text)>,
    mut fills: Query<(&MoveFill, &mut Node), Without<MoveReading>>,
) {
    let mut existing = Vec::with_capacity(roots.iter().len());
    let sampled = session
        .as_ref()
        .zip(snapshots.as_ref())
        .map(|(session, snapshots)| {
            snapshots.sample_mobs(
                Instant::now(),
                Duration::from_secs_f32(1.0 / f32::from(session.0.tick_rate)),
            )
        })
        .unwrap_or_default();
    let position = |one: &PresentedMove| {
        let (camera, transform) = cameras.single().ok()?;
        let (_, mob) = sampled.iter().find(|(id, _)| *id == one.key.boss)?;
        let point = camera
            .world_to_viewport(
                &GlobalTransform::from(*transform),
                mob.pos + Vec3::Y * one.label_height(),
            )
            .ok()?;
        let row = presentation
            .0
            .iter()
            .filter(|entry| entry.key.boss == one.key.boss)
            .position(|entry| entry.key == one.key)
            .unwrap_or(0);
        Some(Vec2::new(
            point.x - WIDTH / 2.0,
            point.y - HEIGHT * (row + 1) as f32,
        ))
    };
    for (entity, root, mut node) in &mut roots {
        let Some(one) = presentation.0.iter().find(|one| one.key == root.0) else {
            commands.entity(entity).despawn();
            continue;
        };
        existing.push(root.0);
        node.display = Display::None;
        if let Some(point) = position(one) {
            node.display = Display::Flex;
            node.left = Val::Px(point.x);
            node.top = Val::Px(point.y);
        }
    }
    for (label, mut text) in &mut labels {
        if let Some(one) = presentation.0.iter().find(|one| one.key == label.0) {
            let next = reading(one);
            if text.0 != next {
                text.0 = next;
            }
        }
    }
    for (fill, mut node) in &mut fills {
        if let Some(one) = presentation.0.iter().find(|one| one.key == fill.0) {
            node.width = Val::Percent(one.progress * 100.0);
        }
    }
    for one in &presentation.0 {
        if existing.contains(&one.key) {
            continue;
        }
        let point = position(one);
        commands
            .spawn((
                MoveReading(one.key),
                Node {
                    position_type: PositionType::Absolute,
                    width: Val::Px(WIDTH),
                    height: Val::Px(HEIGHT),
                    display: if point.is_some() {
                        Display::Flex
                    } else {
                        Display::None
                    },
                    left: Val::Px(point.unwrap_or_default().x),
                    top: Val::Px(point.unwrap_or_default().y),
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.025, 0.035, 0.045, 0.92)),
                GlobalZIndex(11),
            ))
            .with_children(|root| {
                root.spawn((
                    MoveLabel(one.key),
                    Text::new(reading(one)),
                    TextFont {
                        font_size: FontSize::Px(12.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));
                root.spawn((
                    Node {
                        height: Val::Px(4.0),
                        width: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.25, 0.25, 0.25)),
                ))
                .with_children(|track| {
                    track.spawn((
                        MoveFill(one.key),
                        Node {
                            height: Val::Percent(100.0),
                            width: Val::Percent(one.progress * 100.0),
                            ..default()
                        },
                        BackgroundColor(Color::WHITE),
                    ));
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::encounters::tests::timeline;

    pub(crate) fn shown() -> PresentedMove {
        let timeline = timeline();
        PresentedMove {
            key: MoveKey {
                encounter: 5,
                boss: 9,
                instance: 11,
            },
            boss_kind: timeline.boss,
            stage: 2,
            announced: timeline.moves[0].clone(),
            window: Window::Current,
            progress: 0.5,
            remaining_ticks: 10,
        }
    }

    #[test]
    fn physical_cast_channel_recovery_and_stale_readings_are_distinct() {
        let mut one = shown();
        assert!(reading(&one).contains("Preparing 50%"));
        one.boss_kind = MobKind::DraugrKing;
        one.announced.kind = EncounterMoveKind::SepulchreSpear;
        assert!(reading(&one).contains("Casting 50%"));
        one.announced.phase = MovePhase::Channel;
        one.announced.pulse = Some((1, 3));
        one.announced.interruptible = true;
        assert!(reading(&one).contains("Channeling 50% - pulse 2/3 [interruptible]"));
        one.remaining_ticks = 1;
        assert!(reading(&one).contains("PULSE ACTIVE"));
        one.announced.phase = MovePhase::Recovery;
        one.announced.pulse = None;
        assert!(reading(&one).contains("Recovering"));
        one.window = Window::AwaitingUpdate;
        assert!(reading(&one).contains("Awaiting server update"));
        assert!(!reading(&one).contains("ACTIVE"));
    }

    #[test]
    fn ui_replaces_the_move_and_removes_the_whole_reading_on_cancellation() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(EncounterUiPlugin);
        app.world_mut().resource_mut::<EncounterPresentation>().0 = vec![shown()];
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&MoveReading>().iter(world).count(), 1);
        let mut next = shown();
        next.key.instance += 1;
        next.announced.kind = EncounterMoveKind::BonebreakerJaws;
        world.resource_mut::<EncounterPresentation>().0 = vec![next];
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&MoveReading>().iter(world).count(), 1);
        assert!(
            world
                .query::<&Text>()
                .iter(world)
                .all(|text| text.0.contains("Bonebreaker jaws"))
        );
        world.resource_mut::<EncounterPresentation>().0.clear();
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&MoveReading>().iter(world).count(), 0);
        assert_eq!(world.query::<&MoveFill>().iter(world).count(), 0);
        assert_eq!(world.query::<&Text>().iter(world).count(), 0);
    }
}
