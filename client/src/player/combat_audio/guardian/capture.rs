//! Offline review exports use the actual synthesis/playback/mixer path. Producing
//! a WAV and measuring it is not a claim that anybody listened to it.
use super::*;
use crate::audio::{
    AudioMixer, Bus, Mixer, Sink, spatial,
    synth::{Playback, Rendering},
};
use crate::net::{EncounterMoveKind as Move, MobAction as Action, MovePhase as Phase};
use crate::player::{SnapshotBuffer, WorldCamera, encounters};
use std::{io::Write, path::Path, sync::Arc};

struct Buffer(Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
}
pub(in super::super) const RATE: u32 = 48_000;

pub(in super::super) fn wav(path: &Path, samples: &[f32]) {
    assert!(
        samples
            .iter()
            .all(|value| value.is_finite() && value.abs() <= 1.0)
    );
    let size = (samples.len() * 2) as u32;
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + size).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16_u32.to_le_bytes()).unwrap();
    file.write_all(&1_u16.to_le_bytes()).unwrap();
    file.write_all(&2_u16.to_le_bytes()).unwrap();
    file.write_all(&RATE.to_le_bytes()).unwrap();
    file.write_all(&(RATE * 4).to_le_bytes()).unwrap();
    file.write_all(&4_u16.to_le_bytes()).unwrap();
    file.write_all(&16_u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&size.to_le_bytes()).unwrap();
    for sample in samples {
        file.write_all(&((*sample * 32767.0).round() as i16).to_le_bytes())
            .unwrap();
    }
}

pub(in super::super) fn review_directory() -> std::path::PathBuf {
    let directory =
        std::env::var("VOXELHEIM_AUDIO_REVIEW_DIR").expect("explicit review output directory");
    std::fs::create_dir_all(&directory).unwrap();
    directory.into()
}

/// Every recipe of one boss catalogue, spaced, through real playback and the mixer.
pub(in super::super) fn catalogue(
    directory: &Path,
    name: &str,
    cues: &[(String, f32, crate::audio::synth::Sound)],
) {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(RATE, 2);
    let audio = AudioMixer::from_shared_for_test(Arc::clone(&mixer));
    let mut output = Vec::new();
    let mut manifest = String::from("start_seconds,end_seconds,cue\n");
    for (label, seconds, sound) in cues {
        let start = output.len() as f64 / f64::from(RATE * 2);
        let baked = sound.bake(*seconds, RATE, 19).unwrap();
        let mut placement = spatial::place(Vec3::ZERO, 0.0, Vec3::new(0.0, 0.0, -3.0), 32.0, 0.0);
        placement.gain *= SOURCE_GAIN;
        let mut playback =
            Playback::start(&audio, Bus::Sfx, Rendering::Baked(baked), placement).unwrap();
        for _ in 0..((seconds + 0.35) * 100.0).ceil() as usize {
            playback.pump();
            let mut block = Buffer(vec![0.0; (RATE / 100 * 2) as usize]);
            mixer.render(&mut block);
            output.extend(block.0);
        }
        manifest.push_str(&format!(
            "{start:.3},{:.3},{label}\n",
            start + f64::from(*seconds)
        ));
    }
    wav(&directory.join(format!("{name}.wav")), &output);
    std::fs::write(directory.join(format!("{name}.csv")), manifest).unwrap();
    let peak = output.iter().copied().map(f32::abs).fold(0.0, f32::max);
    assert!(peak > 0.05 && peak < 0.99, "catalogue peak {peak}");
    println!(
        "{name}: {} cues at {RATE} Hz, stereo, PCM16; peak {peak:.4}. Manual listening is a separate review.",
        cues.len()
    );
}

#[test]
#[ignore = "writes an offline production-mixer WAV for manual listening; no audio device"]
fn export_guardian_audio_catalogue() {
    let cues = sounds::CUES.map(|cue| (format!("{cue:?}"), cue.seconds(), cue.describe()));
    catalogue(&review_directory(), "guardian-catalogue", &cues);
}

/// A four-block stone wall one block in front of the boss, between it and the listener.
pub(in super::super) fn stone_wall(world: &mut World) {
    world.init_resource::<crate::world::ChunkStore>();
    for cx in [-1, 0] {
        let mut chunk = crate::world::VoxelChunk::all_air(32);
        for x in 0..32 {
            for y in 0..4 {
                chunk.set(x, y, 1, crate::world::palette::STONE);
            }
        }
        world
            .resource_mut::<crate::world::ChunkStore>()
            .insert(crate::net::ChunkCoord { cx, cy: 0, cz: 0 }, chunk);
    }
}

pub(in super::super) struct Recording {
    pub(in super::super) app: App,
    mixer: Arc<Mixer>,
    samples: Vec<f32>,
    manifest: String,
    peak_sources: usize,
}
impl Recording {
    pub(in super::super) fn new(distance: f32, mute: bool) -> Self {
        let (mut app, mixer) = tests::rig_fixture(RATE);
        mixer.set_gain(Bus::Sfx, if mute { 0.0 } else { 1.0 });
        let world = app.world_mut();
        for mut camera in world
            .query_filtered::<&mut Transform, With<WorldCamera>>()
            .iter_mut(world)
        {
            camera.translation = Vec3::new(0.0, 1.25, distance);
        }
        Self {
            app,
            mixer,
            samples: Vec::new(),
            manifest: String::from("seconds,tick,boss,cue,x,y,z\n"),
            peak_sources: 0,
        }
    }
    fn frame(
        &mut self,
        tick: u32,
        action: MobAction,
        position: Vec3,
        timeline: Option<crate::net::EncounterTimeline>,
    ) {
        let mut snapshot = encounters::tests::snapshot(tick);
        snapshot.mobs[0].pos = position.to_array();
        snapshot.mobs[0].action = action;
        self.app
            .world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(
                snapshot,
                std::time::Instant::now() - std::time::Duration::from_millis(100),
            );
        if let Some(timeline) = timeline {
            self.app
                .world_mut()
                .resource_mut::<EncounterTimelineInbox>()
                .push(timeline);
        }
        self.mix(tick);
    }
    pub(in super::super) fn mix(&mut self, tick: u32) {
        self.app.update();
        let state = self.app.world().resource::<super::super::CombatAudio>();
        let seconds = self.samples.len() as f64 / f64::from(RATE * 2);
        for (id, cue, position) in &state.started {
            self.manifest.push_str(&format!(
                "{seconds:.4},{tick},{id},{cue:?},{:.3},{:.3},{:.3}\n",
                position.x, position.y, position.z
            ));
        }
        self.peak_sources = self.peak_sources.max(
            state
                .playing
                .iter()
                .filter(|active| active.owner.is_some())
                .count(),
        );
        let mut buffer = Buffer(vec![0.0; (RATE / 60 * 2) as usize]);
        self.mixer.render(&mut buffer);
        self.samples.extend(buffer.0);
    }
    pub(in super::super) fn save(&self, directory: &Path, name: &str) {
        wav(&directory.join(format!("{name}.wav")), &self.samples);
        std::fs::write(directory.join(format!("{name}.csv")), &self.manifest).unwrap();
        let peak = self
            .samples
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0, f32::max);
        let rms = (self.samples.iter().map(|v| f64::from(v * v)).sum::<f64>()
            / self.samples.len() as f64)
            .sqrt();
        assert!(peak < 0.99, "clipped {name}");
        assert!(self.peak_sources <= MAX_BOSS_SOURCES);
        println!(
            "{name}: peak={peak:.4} rms={rms:.5} boss_sources={}",
            self.peak_sources
        );
    }
}

fn sequence(
    directory: &Path,
    name: &str,
    kind: EncounterMoveKind,
    distance: f32,
    mute: bool,
    paired: bool,
) {
    let mut recording = Recording::new(distance, mute);
    let mut tick = 100;
    let mut position = Vec3::ZERO;
    recording.frame(tick, Action::Idle, position, None);
    tick += 1;
    let combos: Vec<_> = if paired && matches!(kind, Move::BiteAndTear | Move::PrisonerClaws) {
        vec![Some((1, 2)), Some((2, 2))]
    } else {
        vec![None]
    };
    for (index, combo) in combos.into_iter().enumerate() {
        for phase in [Phase::Telegraph, Phase::Release, Phase::Recovery] {
            let mut timeline = tests::timeline(kind, phase, combo, tick);
            timeline.moves[0].move_instance_id = 11 + index as u64;
            if kind == Move::PredatorLeap {
                timeline.moves[0].hazards[0].origin = [0.0, 1.5, -6.5];
            }
            let ticks = timeline.moves[0].phase_ticks;
            for elapsed in 0..ticks {
                if phase == Phase::Release {
                    if kind == Move::CollarCharge {
                        position.z -= 11.0 / 60.0;
                    }
                    if kind == Move::PredatorLeap {
                        position.z = (-12.0 * (elapsed + 1) as f32 / 60.0).max(-6.5);
                    }
                }
                recording.frame(
                    tick,
                    if phase == Phase::Recovery {
                        Action::Recovery
                    } else {
                        Action::Windup
                    },
                    position,
                    (elapsed == 0).then(|| timeline.clone()),
                );
                tick += 1;
            }
        }
    }
    let mut empty = tests::timeline(kind, Phase::Recovery, None, tick);
    empty.moves.clear();
    for elapsed in 0..60 {
        recording.frame(
            tick,
            Action::Idle,
            position,
            (elapsed == 0).then(|| empty.clone()),
        );
        tick += 1;
    }
    recording.save(directory, name);
}

#[test]
#[ignore = "writes actual ECS/mixer sequence WAVs and timestamps for manual listening"]
fn export_guardian_audio_sequences() {
    let directory =
        std::env::var("VOXELHEIM_AUDIO_REVIEW_DIR").expect("explicit review output directory");
    let directory = Path::new(&directory);
    std::fs::create_dir_all(directory).unwrap();
    for (name, kind) in [
        ("bite-combo", Move::BiteAndTear),
        ("claw-combo", Move::PrisonerClaws),
        ("charge", Move::CollarCharge),
        ("leap", Move::PredatorLeap),
        ("heavy-jaws", Move::BonebreakerJaws),
    ] {
        sequence(directory, name, kind, 3.0, false, true);
    }
    sequence(
        directory,
        "claws-13-blocks",
        Move::PrisonerClaws,
        13.0,
        false,
        true,
    );
    sequence(
        directory,
        "claws-25-blocks",
        Move::PrisonerClaws,
        25.0,
        false,
        true,
    );
    sequence(
        directory,
        "claws-muted",
        Move::PrisonerClaws,
        3.0,
        true,
        true,
    );

    sequence(
        directory,
        "single-claw",
        Move::PrisonerClaws,
        3.0,
        false,
        false,
    );
    review_spatial_and_party(directory);

    let mut recording = Recording::new(3.0, false);
    for tick in 100..280 {
        let action = if tick == 100 {
            Action::Idle
        } else if tick >= 220 {
            Action::Corpse
        } else {
            Action::Chase
        };
        let position = Vec3::new(0.0, 0.0, -((tick.min(200) - 100) as f32) / 60.0);
        let timeline = if tick == 100 || tick == 200 {
            let mut state = tests::timeline(Move::BiteAndTear, Phase::Recovery, None, tick);
            state.phase = if tick == 100 { 1 } else { 2 };
            state.moves.clear();
            Some(state)
        } else {
            None
        };
        recording.frame(tick, action, position, timeline);
    }
    recording.save(directory, "notice-walk-phase-death");

    let mut recording = Recording::new(3.0, false);
    for tick in 100..240 {
        let timeline = match tick {
            100 => Some(tests::timeline(
                Move::BonebreakerJaws,
                Phase::Telegraph,
                None,
                100,
            )),
            112 => {
                let mut state = tests::timeline(Move::BonebreakerJaws, Phase::Telegraph, None, 100);
                state.moves[0].ended = Some(crate::net::MoveEnd::Cancelled);
                Some(state)
            }
            140 => {
                let mut state =
                    tests::timeline(Move::PrisonerClaws, Phase::Telegraph, Some((2, 2)), 125);
                state.moves[0].move_instance_id = 12;
                Some(state)
            } // Too late for the raised-paw sound.
            185 => {
                let mut state =
                    tests::timeline(Move::PrisonerClaws, Phase::Release, Some((2, 2)), 185);
                state.moves[0].move_instance_id = 12;
                Some(state)
            }
            190 => {
                let mut state =
                    tests::timeline(Move::BiteAndTear, Phase::Telegraph, Some((1, 2)), 190);
                state.moves[0].move_instance_id = 13;
                Some(state)
            }
            _ => None,
        };
        recording.frame(tick, Action::Windup, Vec3::ZERO, timeline);
    }
    recording.save(directory, "cancel-late-second-replaced");
}

fn review_spatial_and_party(directory: &Path) {
    for (name, eye_x, wall) in [
        ("left-listener", 3.0, false),
        ("right-listener", -3.0, false),
        ("jaws-open-room", 0.0, false),
        ("jaws-stone-wall", 0.0, true),
    ] {
        let mut recording = Recording::new(3.0, false);
        let world = recording.app.world_mut();
        for mut eye in world
            .query_filtered::<&mut Transform, With<WorldCamera>>()
            .iter_mut(world)
        {
            eye.translation.x = eye_x;
        }
        if wall {
            stone_wall(world);
        }
        for tick in 100..220 {
            recording.frame(
                tick,
                Action::Windup,
                Vec3::ZERO,
                (tick == 100)
                    .then(|| tests::timeline(Move::BonebreakerJaws, Phase::Telegraph, None, 100)),
            );
        }
        recording.save(directory, name);
    }
    let mut recording = Recording::new(3.0, false);
    let audio = AudioMixer::from_shared_for_test(Arc::clone(&recording.mixer));
    // Eight synthetic steady reference tones prove protected voice coexistence. They
    // are QA signals, not recorded speech, and are labelled as such in the manifest.
    let mut voices = Vec::new();
    for index in 0..8 {
        use crate::audio::synth::{Envelope, Exciter, Layer, Sound, Wave};
        let sound = Sound {
            layers: vec![Layer {
                exciter: Exciter::Oscillator {
                    wave: Wave::Sine,
                    hz: 220.0 + index as f32 * 29.0,
                },
                gain: 0.015,
                envelope: Envelope {
                    attack: 0.02,
                    decay: 0.0,
                    sustain: 1.0,
                    release: 0.02,
                },
                gate: None,
                filter: None,
            }],
        };
        voices.push(
            Playback::start(
                &audio,
                Bus::Voice,
                Rendering::Baked(sound.bake(3.0, RATE, 19).unwrap()),
                spatial::place(Vec3::ZERO, 0.0, Vec3::new(0.0, 0.0, -1.0), 32.0, 0.0),
            )
            .unwrap(),
        );
    }
    for tick in 100..280 {
        let mut snapshot = encounters::tests::snapshot(tick);
        let original = snapshot.mobs[0];
        snapshot.mobs.clear();
        for id in 9..13 {
            let mut mob = original;
            mob.entity_id = id;
            mob.pos[0] = (id - 9) as f32 * 0.5;
            snapshot.mobs.push(mob);
            if tick == 100 {
                let mut state =
                    tests::timeline(Move::BonebreakerJaws, Phase::Telegraph, None, tick);
                state.encounter_id = id;
                state.boss_entity_id = id;
                recording
                    .app
                    .world_mut()
                    .resource_mut::<EncounterTimelineInbox>()
                    .push(state);
            }
        }
        recording
            .app
            .world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(
                snapshot,
                std::time::Instant::now() - std::time::Duration::from_millis(100),
            );
        for voice in &mut voices {
            voice.pump();
        }
        recording.mix(tick);
    }
    recording
        .manifest
        .push_str("0.0000,100,0,QA-eight-reference-voice-tones,0,0,0\n");
    recording.save(directory, "party-four-bosses-eight-reference-voices");
}
