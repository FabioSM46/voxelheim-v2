use super::tests::sound;
use super::*;
use crate::audio::{AudioMixer, Bus, Mixer, SOURCE_CAPACITY, mixer::Sink, spatial};
use bevy::prelude::Vec3;
use std::sync::Arc;

struct Buffer(Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
}
fn close_samples(got: &[f32], want: &[f32]) {
    assert_eq!(got.len(), want.len());
    for (index, (got, want)) in got.iter().zip(want).enumerate() {
        // The mixer's identity band split still adds/subtracts in f32.
        assert!((got - want).abs() < 1e-6, "sample {index}: {got} != {want}");
    }
}

fn listen(mixer: &Mixer, count: usize) -> Vec<f32> {
    let mut sink = Buffer(vec![0.0; count]);
    mixer.render(&mut sink);
    sink.0
}

#[test]
fn continuous_samples_do_not_depend_on_chunking_and_do_not_repeat_two_second_windows() {
    let described = sound(Exciter::Noise(Noise::Brown));
    let mut whole = described.continuous(8_000, 42).unwrap();
    let mut chunks = described.continuous(8_000, 42).unwrap();
    let mut output = vec![0.0; 8_000 * 60];
    assert_eq!(whole.render(&mut output), output.len());
    let mut chunked = vec![0.0; output.len()];
    for block in chunked.chunks_mut(137) {
        assert_eq!(chunks.render(block), block.len());
    }
    assert_eq!(output, chunked);
    let windows: Vec<_> = output.chunks_exact(16_000).collect();
    for (index, window) in windows.iter().enumerate() {
        for other in &windows[index + 1..] {
            assert_ne!(window, other);
        }
    }
    let mut another = described.continuous(8_000, 43).unwrap();
    let mut different = vec![0.0; output.len()];
    another.render(&mut different);
    assert_ne!(output, different);
}

#[test]
fn continuous_stop_is_silent_idempotent_and_works_during_attack() {
    for rate in [8_000, 44_100, 48_000, 96_000] {
        for before in [0, 1, 10, rate as usize] {
            let mut described = sound(Exciter::Noise(Noise::White));
            described.layers[0].filter = Some(Filter {
                kind: FilterKind::Low,
                hz: 800.0,
                q: 0.7,
            });
            let mut continuous = described.continuous(rate, 7).unwrap();
            let mut start = vec![0.0; before];
            continuous.render(&mut start);
            if before > 0 {
                assert_eq!(start[0], 0.0);
            }
            continuous.stop();
            continuous.stop();
            let mut tail = vec![f32::NAN; rate as usize];
            let count = continuous.render(&mut tail);
            assert_eq!(
                count,
                (f64::from(described.layers[0].envelope.release) * f64::from(rate)).ceil() as usize
                    + 1
            );
            assert_eq!(tail[count - 1], 0.0);
            assert!(tail[..count].iter().all(|v| v.is_finite()));
            if before == 0 {
                assert!(tail[..count].iter().all(|v| *v == 0.0));
            }
            assert_eq!(continuous.render(&mut tail), 0);
        }
    }
}

#[test]
fn a_pure_tone_cannot_be_mistaken_for_a_nonrepeating_bed() {
    let tone = sound(Exciter::Oscillator {
        wave: Wave::Sine,
        hz: 440.0,
    });
    assert_eq!(
        tone.continuous(48_000, 0).unwrap_err(),
        Error::ContinuousNeedsNoise
    );
    let mut silent = sound(Exciter::Noise(Noise::White));
    silent.layers[0].gain = 0.0;
    assert_eq!(
        silent.continuous(48_000, 0).unwrap_err(),
        Error::ContinuousNeedsNoise
    );
}

#[test]
fn a_gated_layer_cannot_be_mistaken_for_a_nonrepeating_bed() {
    // Noise, audible and sustained, so the noise check passes on its own — the gate is the
    // only thing wrong with it, and once it winds up to `to` it strikes strictly periodically.
    let mut gated = sound(Exciter::Noise(Noise::White));
    gated.layers[0].gate = Some(Gate {
        from: 40.0,
        to: 40.0,
        seconds: 0.1,
        curve: Curve::Linear,
        duty: 0.2,
    });
    assert_eq!(gated.continuous(48_000, 0).unwrap_err(), Error::GatedBed);
    // The same description bakes: a struck sound is where a gate belongs.
    assert!(gated.bake(1.0, 48_000, 0).is_ok());
    // And the bed it was made from is still a bed once the gate is gone.
    gated.layers[0].gate = None;
    assert!(gated.continuous(48_000, 0).is_ok());
}

#[test]
fn baked_playback_preserves_samples_across_ring_refills_and_drains_the_last_sample() {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(44_100, 1);
    let audio = AudioMixer(Arc::clone(&mixer));
    let baked = sound(Exciter::Noise(Noise::White))
        .bake(0.8, 44_100, 42)
        .unwrap();
    let mut playing = Playback::start(
        &audio,
        Bus::Sfx,
        Rendering::Baked(baked.clone()),
        spatial::Placement::UNPOSITIONED,
    )
    .unwrap();
    assert_eq!(playing.pump(), Status::Playing);
    // A full ring must not advance the baked cursor.
    assert_eq!(playing.pump(), Status::Playing);
    let mut output = Vec::new();
    while output.len() < baked.samples().len() {
        let count = 613.min(baked.samples().len() - output.len());
        output.extend(listen(&mixer, count));
        if output.len() < baked.samples().len() {
            assert_eq!(playing.pump(), Status::Playing);
        }
    }
    close_samples(&output, baked.samples());
    assert_eq!(playing.pump(), Status::Finished);
    assert_eq!(playing.pump(), Status::Finished);
    listen(&mixer, 1);
    let handles: Vec<_> = (0..crate::audio::mixer::MAX_SOURCES)
        .filter_map(|_| mixer.claim(Bus::Voice))
        .collect();
    assert_eq!(handles.len(), crate::audio::mixer::MAX_SOURCES);
}

#[test]
fn continuous_playback_matches_direct_render_and_gracefully_stops() {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(8_000, 1);
    let audio = AudioMixer(Arc::clone(&mixer));
    let described = sound(Exciter::Noise(Noise::Brown));
    let mut direct = described.continuous(8_000, 7).unwrap();
    let mut playback = Playback::start(
        &audio,
        Bus::Ambience,
        Rendering::Continuous(described.continuous(8_000, 7).unwrap()),
        spatial::Placement::UNPOSITIONED,
    )
    .unwrap();
    playback.pump();
    let mut want = vec![0.0; SOURCE_CAPACITY];
    direct.render(&mut want);
    close_samples(&listen(&mixer, SOURCE_CAPACITY), &want);
    playback.stop();
    direct.stop();
    let mut tail = vec![0.0; 8000];
    let n = direct.render(&mut tail);
    assert_eq!(playback.pump(), Status::Playing);
    close_samples(&listen(&mixer, n), &tail[..n]);
    assert_eq!(playback.pump(), Status::Finished);
}

#[test]
fn existing_spatial_placement_and_sfx_gain_are_heard_through_the_sink() {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(48_000, 2);
    mixer.set_gain(Bus::Sfx, 0.5);
    let audio = AudioMixer(Arc::clone(&mixer));
    let baked = sound(Exciter::Noise(Noise::White))
        .bake(0.1, 48_000, 42)
        .unwrap();
    let placement = spatial::place(Vec3::ZERO, 0.0, Vec3::X * 2.0, 16.0, 0.0);
    let mut playback =
        Playback::start(&audio, Bus::Sfx, Rendering::Baked(baked.clone()), placement).unwrap();
    playback.pump();
    let out = listen(&mixer, baked.samples().len() * 2);
    let reference = Arc::new(Mixer::new());
    reference.set_format(48_000, 2);
    let source = reference.claim(Bus::Master).unwrap();
    source.place(placement);
    source.push(baked.samples());
    let reference = listen(&reference, out.len());
    let mut right = 0.0;
    let mut left = 0.0;
    for (frame, expected) in out.chunks_exact(2).zip(reference.chunks_exact(2)) {
        left += frame[0].abs();
        right += frame[1].abs();
        assert!((frame[1] - expected[1] * 0.5).abs() < 1e-6);
    }
    assert!(right > 1.0 && left < right * 0.001);
}

#[test]
fn revoked_and_reclocked_sources_release_their_slots_without_retrying() {
    for revoke in [false, true] {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        let audio = AudioMixer(Arc::clone(&mixer));
        let mut playback = Playback::start(
            &audio,
            Bus::Music,
            Rendering::Continuous(
                sound(Exciter::Noise(Noise::White))
                    .continuous(48_000, 0)
                    .unwrap(),
            ),
            spatial::Placement::UNPOSITIONED,
        )
        .unwrap();
        playback.pump();
        let expected = if revoke {
            mixer.set_enabled(Bus::Music, false);
            Status::Revoked
        } else {
            mixer.set_format(44_100, 1);
            Status::RateChanged
        };
        // The callback must already be silent BEFORE the producer sees the rate change.
        assert!(listen(&mixer, 512).iter().all(|v| *v == 0.0));
        assert_eq!(playback.pump(), expected);
        assert!(listen(&mixer, 512).iter().all(|v| *v == 0.0));
        assert_eq!(playback.pump(), expected);
        let handles: Vec<_> = (0..crate::audio::mixer::MAX_SOURCES)
            .filter_map(|_| mixer.claim(Bus::Voice))
            .collect();
        assert_eq!(handles.len(), crate::audio::mixer::MAX_SOURCES);
        // Recycling must remove the rate restriction for a later unbound voice owner.
        for handle in &handles {
            handle.push(&[0.01]);
        }
        assert!((listen(&mixer, 1)[0] - handles.len() as f32 * 0.01).abs() < 1e-6);
    }
}

#[test]
fn stale_baked_rates_and_full_pools_refuse_immediately() {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(48_000, 1);
    let audio = AudioMixer(Arc::clone(&mixer));
    let described = sound(Exciter::Noise(Noise::White));
    let stale = described.bake(0.1, 44_100, 0).unwrap();
    assert_eq!(
        Playback::start(
            &audio,
            Bus::Sfx,
            Rendering::Baked(stale),
            spatial::Placement::UNPOSITIONED
        )
        .unwrap_err(),
        StartError::RateChanged
    );
    let held: Vec<_> = (0..crate::audio::mixer::MAX_SOURCES - crate::audio::mixer::VOICE_RESERVE)
        .map(|_| mixer.claim(Bus::Sfx).unwrap())
        .collect();
    assert_eq!(
        Playback::start(
            &audio,
            Bus::Sfx,
            Rendering::Baked(described.bake(0.1, 48_000, 0).unwrap()),
            spatial::Placement::UNPOSITIONED
        )
        .unwrap_err(),
        StartError::NoSlot
    );
    assert!(held.iter().all(|h| h.live()));
}

#[test]
#[ignore = "manual CPU measurement; no timing assertion on shared CI hosts"]
fn continuous_cpu_budget() {
    for layers in [1, MAX_LAYERS] {
        let mut described = sound(Exciter::Noise(Noise::Brown));
        described.layers[0].filter = Some(Filter {
            kind: FilterKind::Low,
            hz: 1000.0,
            q: 0.7,
        });
        described.layers = vec![described.layers[0].clone(); layers];
        for rate in [44_100, 48_000, 96_000] {
            let mut source = described.continuous(rate, 42).unwrap();
            let mut block = vec![0.0; rate as usize / 50];
            let started = std::time::Instant::now();
            for _ in 0..3000 {
                source.render(std::hint::black_box(&mut block));
                std::hint::black_box(&block);
            }
            let elapsed = started.elapsed().as_secs_f64();
            println!(
                "layers={layers} rate={rate}: 60 audio seconds in {elapsed:.6} CPU-wall seconds; {:.3} ms per 20ms block; {:.3}% of one core",
                elapsed / 3.0,
                elapsed / 60.0 * 100.0
            );
        }
    }
}
