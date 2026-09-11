//! Reduced effects removes decoration and never information (#1093). Two identical
//! headless clients receive the same snapshots and timelines in lockstep, one with the
//! setting on. On every step the essential cue set is the same — the authoritative
//! presentation, the hazard boundaries with their contact lines, the encounter readings
//! and every body part's pose — and only the optional flourishes differ: spell shapes,
//! the hand crystal and the core's brightening on contact.

use super::king::regalia::flourishes;
use super::tests::{deliver, draugr, headless};
use super::*;
use crate::net::{
    EncounterMoveKind::{self, *},
    EncounterTimeline, EncounterTimelineInbox, HazardShape, HazardVolume, MobState,
    MovePhase::{self, *},
};
use crate::player::encounters::{self, EncounterPresentation, MoveKey, ReducedEffects};
use bevy::time::TimeUpdateStrategy;

const KING: u64 = 900;
const VARGR: u64 = 901;
/// Bodies are interpolated against the wall clock, which two clients never share exactly.
const POSE_TOLERANCE: f32 = 1e-4;

/// Everything a player needs to read a move, which must not depend on the setting.
#[derive(Debug)]
struct Essential {
    presentation: Vec<encounters::PresentedMove>,
    cues: Vec<(MoveKey, usize, HazardVolume, bool, Transform)>,
    readings: Vec<String>,
    poses: Vec<(u64, String, Transform, Option<Visibility>)>,
}

impl Essential {
    /// Exact for everything announced, drawn or written; poses within the tolerance.
    fn matches(&self, other: &Self) -> bool {
        self.presentation == other.presentation
            && self.cues == other.cues
            && self.readings == other.readings
            && self.poses.len() == other.poses.len()
            && self.poses.iter().zip(&other.poses).all(|(a, b)| {
                (&a.0, &a.1, a.3) == (&b.0, &b.1, b.3)
                    && a.2.translation.abs_diff_eq(b.2.translation, POSE_TOLERANCE)
                    && a.2.rotation.abs_diff_eq(b.2.rotation, POSE_TOLERANCE)
                    && a.2.scale.abs_diff_eq(b.2.scale, POSE_TOLERANCE)
            })
    }
}

/// What the setting may withhold.
#[derive(Debug, PartialEq)]
struct Optional {
    groups: Vec<MoveKey>,
    regalia: Option<(&'static str, bool, f32)>,
}

fn essential(app: &mut App) -> Essential {
    let world = app.world_mut();
    let presentation = world.resource::<EncounterPresentation>().0.clone();
    let cues = encounters::boundary_cues(world)
        .into_iter()
        .map(|(key, index, volume, contact, placed, _)| (key, index, volume, contact, placed))
        .collect();
    let mut readings: Vec<String> = world
        .query::<&Text>()
        .iter(world)
        .map(|text| text.0.clone())
        .collect();
    readings.sort();
    let mut poses: Vec<_> = world
        .query::<(&MobVisual, &Transform, Option<&Visibility>)>()
        .iter(world)
        .map(|(part, placed, visibility)| {
            let owner = world.get::<Mob>(part.owner).map_or(0, |mob| mob.entity_id);
            (
                owner,
                format!("{:?}", part.part),
                *placed,
                visibility.copied(),
            )
        })
        .collect();
    poses.extend(
        world
            .query::<(&Mob, &Transform)>()
            .iter(world)
            .map(|(mob, placed)| (mob.entity_id, "Root".to_owned(), *placed, None)),
    );
    poses.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    Essential {
        presentation,
        cues,
        readings,
        poses,
    }
}

fn optional(app: &mut App) -> Optional {
    let world = app.world_mut();
    let mut groups = encounters::effect_groups(world);
    groups.sort_by_key(|key| (key.encounter, key.boss, key.instance));
    Optional {
        groups,
        regalia: flourishes(world),
    }
}

fn client(reduced: bool) -> App {
    let mut app = headless();
    // The real propagation, so an effect group counts only what would actually be drawn.
    app.add_plugins((
        crate::ui::encounters::EncounterUiPlugin,
        bevy::mesh::MeshPlugin,
        bevy::camera::visibility::VisibilityPlugin,
    ))
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        50,
    )))
    .insert_resource(ReducedEffects(reduced));
    app
}

fn boss(kind: MobKind) -> MobState {
    let id = if kind == MobKind::DraugrKing {
        KING
    } else {
        VARGR
    };
    MobState {
        kind,
        ..draugr(id, 3.0, 100, MobAction::Windup)
    }
}

/// One announced phase on its own instance, with the server's shape for that move.
fn timeline(
    kind: MobKind,
    stage: u8,
    what: EncounterMoveKind,
    phase: MovePhase,
    started: u32,
    ticks: u32,
    pulse: Option<(u8, u8)>,
) -> EncounterTimeline {
    let volume = |shape, origin: [f32; 3], radius, height| HazardVolume {
        shape,
        origin,
        direction: [0.0, 0.0, -1.0],
        radius,
        height,
    };
    let hazards = match what {
        SepulchreSpear => vec![volume(
            HazardShape::Line { half_width: 0.9 },
            [3.0, 65.4, 0.0],
            17.6,
            2.6,
        )],
        Burial => vec![volume(
            HazardShape::Ring { inner_radius: 2.0 },
            [3.0, 65.0, 0.0],
            4.0,
            2.0,
        )],
        EdictOfTheGraves | RequiemOfTheBuried => [-4.0, 10.0]
            .map(|x| volume(HazardShape::Disc, [x, 65.2, 1.0], 3.0, 2.4))
            .to_vec(),
        PrisonerClaws => vec![volume(
            HazardShape::Cone { half_angle: 1.05 },
            [3.0, 64.9, 0.0],
            3.4,
            2.2,
        )],
        _ => vec![volume(
            HazardShape::Line { half_width: 1.4 },
            [3.0, 65.2, 0.0],
            9.9,
            2.4,
        )],
    };
    let mut timeline = encounters::tests::timeline();
    (timeline.boss, timeline.boss_entity_id, timeline.phase) = (kind, boss(kind).entity_id, stage);
    let one = &mut timeline.moves[0];
    one.move_instance_id = u64::from(started);
    (one.kind, one.phase, one.phase_started_tick, one.phase_ticks) = (what, phase, started, ticks);
    (one.pulse, one.hazards) = (pulse, hazards);
    timeline
}

/// Two clients fed in lockstep: `off` never has reduced effects, `on` has the setting
/// given here and may be switched while they run.
struct Pair {
    off: App,
    on: App,
}

impl Pair {
    fn new(on: bool) -> Self {
        Self {
            off: client(false),
            on: client(on),
        }
    }

    fn announce(&mut self, timeline: &EncounterTimeline) {
        for app in [&mut self.off, &mut self.on] {
            app.world_mut()
                .resource_mut::<EncounterTimelineInbox>()
                .push(timeline.clone());
        }
    }

    /// Both clients show the same essential set, which is returned.
    fn compare(&mut self, at: &str) -> Essential {
        let shown = essential(&mut self.off);
        let other = essential(&mut self.on);
        assert!(
            shown.matches(&other),
            "{at}: reduced effects changed an essential cue\n{shown:?}\n{other:?}"
        );
        shown
    }

    /// One snapshot and three frames on both.
    fn step(&mut self, tick: u32, mob: MobState) -> (Essential, Optional, Optional) {
        for app in [&mut self.off, &mut self.on] {
            deliver(app, tick, vec![mob]);
            for _ in 0..3 {
                app.update();
            }
        }
        let shown = self.compare(&format!("tick {tick}"));
        (shown, optional(&mut self.off), optional(&mut self.on))
    }

    /// Switches `on` with no new announcement or snapshot, then runs one frame on both.
    fn switch(&mut self, reduced: bool) -> Essential {
        self.on.world_mut().resource_mut::<ReducedEffects>().0 = reduced;
        for app in [&mut self.off, &mut self.on] {
            app.update();
        }
        self.compare(&format!("switched to {reduced}"))
    }
}

#[test]
fn every_draugr_spell_keeps_its_essential_cues_and_only_loses_flourishes() {
    let mut pair = Pair::new(true);
    let king = boss(MobKind::DraugrKing);
    for (stage, what, phase, started, ticks, pulse, offsets) in [
        (
            1,
            SepulchreSpear,
            Telegraph,
            1000,
            28,
            None,
            &[0, 14, 27][..],
        ),
        (1, SepulchreSpear, Release, 1028, 16, None, &[0, 7, 15][..]),
        (2, Burial, Telegraph, 1100, 30, None, &[0, 15][..]),
        (2, Burial, Channel, 1130, 14, Some((0, 4)), &[0, 6, 13][..]),
        (2, EdictOfTheGraves, Telegraph, 1150, 30, None, &[10][..]),
        (
            2,
            EdictOfTheGraves,
            Channel,
            1180,
            16,
            Some((1, 3)),
            &[0, 15][..],
        ),
        (
            3,
            RequiemOfTheBuried,
            Telegraph,
            1200,
            30,
            None,
            &[0, 15][..],
        ),
        (
            3,
            RequiemOfTheBuried,
            Channel,
            1230,
            18,
            Some((2, 3)),
            &[0, 17][..],
        ),
    ] {
        let timeline = timeline(
            MobKind::DraugrKing,
            stage,
            what,
            phase,
            started,
            ticks,
            pulse,
        );
        pair.announce(&timeline);
        for &offset in offsets {
            let at = format!("{what:?}/{phase:?}+{offset}");
            let (shown, off, on) = pair.step(started + offset, king);
            assert_eq!(shown.cues.len(), timeline.moves[0].hazards.len(), "{at}");
            assert!(
                shown
                    .readings
                    .iter()
                    .any(|text| text.contains("Draugr king")),
                "{at}: no reading"
            );
            // The server's contact: every release tick, and a channel pulse's last tick.
            let contact = phase == Release || (phase == Channel && offset + 1 == ticks);
            assert!(shown.cues.iter().all(|cue| cue.3 == contact), "{at}");

            // Reduced: no spell shape, no hand crystal, and a core that never brightens.
            assert!(on.groups.is_empty(), "{at}: a spell shape stayed");
            let (core, crystal, _) = on.regalia.expect("the king is dressed");
            assert!(!crystal, "{at}: the hand crystal stayed");
            let steady = if stage >= 3 { "lit" } else { "hidden" };
            assert_eq!(
                core, steady,
                "{at}: the final stage's light is the stage itself"
            );

            // The same step with every flourish, so the comparison above is not vacuous.
            let (core, crystal, _) = off.regalia.expect("the king is dressed");
            let forming = what == SepulchreSpear && phase == Telegraph;
            assert_eq!(crystal, forming, "{at}");
            assert_eq!(core, if contact { "flare" } else { steady }, "{at}");
            assert_eq!(
                off.groups.is_empty(),
                forming,
                "{at}: a spell drew no shape"
            );
        }
    }
}

#[test]
fn a_vargr_move_reads_and_draws_the_same_in_both_modes() {
    let mut pair = Pair::new(true);
    let vargr = boss(MobKind::VargrGuardian);
    for (what, phase, started, ticks, offsets) in [
        (CollarCharge, Telegraph, 2000, 72, &[0, 36][..]),
        (CollarCharge, Release, 2072, 54, &[0, 53][..]),
        (PrisonerClaws, Telegraph, 2200, 60, &[30][..]),
        (PrisonerClaws, Release, 2260, 18, &[0, 17][..]),
    ] {
        let timeline = timeline(MobKind::VargrGuardian, 2, what, phase, started, ticks, None);
        pair.announce(&timeline);
        for &offset in offsets {
            let at = format!("{what:?}/{phase:?}+{offset}");
            let (shown, off, on) = pair.step(started + offset, vargr);
            assert_eq!(shown.cues.len(), 1, "{at}");
            assert!(
                shown.readings.iter().any(|text| text.contains("Vargr")),
                "{at}"
            );
            // Nothing the Vargr shows is a flourish this setting withholds.
            assert_eq!(off, on, "{at}");
            assert_eq!(
                off.groups.is_empty(),
                !(what == PrisonerClaws && phase == Release),
                "{at}: the planted claws draw their reach in both modes"
            );
        }
    }
}

#[test]
fn switching_mid_channel_neither_duplicates_nor_loses_a_cue() {
    let mut pair = Pair::new(false);
    let king = boss(MobKind::DraugrKing);
    let meshes = |app: &App| app.world().resource::<Assets<Mesh>>().len();
    let cue_entities = |app: &mut App| -> Vec<Entity> {
        encounters::boundary_cues(app.world_mut())
            .into_iter()
            .map(|cue| cue.5)
            .collect()
    };
    let core =
        |app: &mut App| flourishes(app.world_mut()).map(|(core, crystal, _)| (core, crystal));
    // Stage 2 first, so the final stage below is a witnessed transition as in a real pull.
    pair.announce(&timeline(
        MobKind::DraugrKing,
        2,
        Burial,
        Telegraph,
        1000,
        30,
        None,
    ));
    pair.step(1010, king);
    let requiem = |pulse| {
        timeline(
            MobKind::DraugrKing,
            3,
            RequiemOfTheBuried,
            Channel,
            1100,
            18,
            Some((pulse, 3)),
        )
    };
    pair.announce(&requiem(1));
    pair.step(1108, king);
    let drawn = meshes(&pair.on);
    let cues = cue_entities(&mut pair.on);
    assert_eq!(cues.len(), 2);
    assert_eq!(optional(&mut pair.on).groups.len(), 1);

    // On, mid-pulse: the next frame withholds both spell shapes and releases their
    // meshes, while every cue stays the same entity and matches the unswitched client.
    pair.switch(true);
    assert_eq!(cue_entities(&mut pair.on), cues);
    assert_eq!(meshes(&pair.on), drawn - 2);
    assert!(optional(&mut pair.on).groups.is_empty());
    assert_eq!(core(&mut pair.on), Some(("lit", false)));

    // The contact tick still arrives on the same cues, with its double line and reading.
    let (contact, off, on) = pair.step(1117, king);
    assert!(contact.cues.iter().all(|cue| cue.3));
    assert!(
        contact
            .readings
            .iter()
            .any(|text| text.contains("PULSE ACTIVE"))
    );
    assert_eq!((off.groups.len(), on.groups.len()), (1, 0));
    assert_eq!(cue_entities(&mut pair.on), cues);
    assert_eq!(meshes(&pair.on), drawn - 2);
    assert_eq!(core(&mut pair.on), Some(("lit", false)));

    // Off on that same contact tick: the shapes and the flare return once, not twice.
    pair.switch(false);
    assert_eq!(cue_entities(&mut pair.on), cues);
    assert_eq!(meshes(&pair.on), drawn);
    assert_eq!(optional(&mut pair.on), optional(&mut pair.off));
    assert_eq!(core(&mut pair.on), Some(("flare", false)));

    // Repeated switching grows nothing and loses nothing.
    for reduced in [true, false, true, false, true, false] {
        pair.switch(reduced);
        assert_eq!(cue_entities(&mut pair.on), cues);
    }
    assert_eq!(meshes(&pair.on), drawn);
    assert_eq!(optional(&mut pair.on), optional(&mut pair.off));
}
