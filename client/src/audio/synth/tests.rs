use super::*;
use std::f64::consts::TAU;

/// #1161: a call of separate syllables. Each strike lands at its onset as exactly the buffer
/// `bake` gives for its own seed, with exact silence between and after, and a strike that
/// cannot end inside the buffer is refused rather than cut short.
#[test]
fn a_sound_struck_at_several_onsets_is_each_strike_in_place_with_silence_between() {
    let described = sound(Exciter::Noise(Noise::White));
    let (length, rate) = (0.05, 8000);
    let call = described
        .bake_at(&[0.0, 0.2], length, 0.3, rate, 9)
        .unwrap();
    let samples = call.samples();
    assert_eq!((samples.len(), call.sample_rate()), (2400, rate));
    let first = described.bake(length, rate, 9).unwrap();
    let second = described.bake(length, rate, 10).unwrap();
    assert_eq!(&samples[..400], first.samples());
    assert_eq!(&samples[1600..2000], second.samples());
    assert_ne!(first.samples(), second.samples(), "one grain replayed");
    assert!(
        samples[400..1600]
            .iter()
            .chain(&samples[2000..])
            .all(|v| *v == 0.0)
    );
    for (onsets, seconds, error) in [
        (&[0.26][..], 0.3, Error::Duration),
        (&[-0.01][..], 0.3, Error::Duration),
        (&[0.0][..], 0.04, Error::Duration),
        (&[][..], 0.3, Error::Layers),
        (&[0.0; MAX_LAYERS + 1][..], 0.3, Error::Layers),
    ] {
        assert_eq!(
            described
                .bake_at(onsets, length, seconds, rate, 9)
                .unwrap_err(),
            error
        );
    }
}

pub(super) fn sound(exciter: Exciter) -> Sound {
    Sound {
        layers: vec![Layer {
            exciter,
            gain: 0.4,
            envelope: Envelope {
                attack: 0.005,
                decay: 0.01,
                sustain: 0.7,
                release: 0.01,
            },
            filter: None,
        }],
    }
}

#[test]
fn oscillator_period_and_shape_at_several_device_rates() {
    for rate in [8_000, 44_100, 48_000, 96_000] {
        for wave in [Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle] {
            let mut generator = Generator::new(
                Exciter::Oscillator {
                    wave,
                    hz: rate as f32 / 16.0,
                },
                7,
                rate,
            );
            let first: Vec<_> = (0..16).map(|_| generator.next()).collect();
            let second: Vec<_> = (0..16).map(|_| generator.next()).collect();
            for (a, b) in first.iter().zip(second) {
                assert!((a - b).abs() < 1e-12);
            }
            let want = match wave {
                Wave::Sine => [0.0, 1.0, 0.0, -1.0],
                Wave::Saw => [-1.0, -0.5, 0.0, 0.5],
                Wave::Square => [1.0, 1.0, -1.0, -1.0],
                Wave::Triangle => [-1.0, 0.0, 1.0, 0.0],
            };
            for (index, want) in [0, 4, 8, 12].into_iter().zip(want) {
                assert!((first[index] - want).abs() < 1e-12);
            }
        }
    }
}

#[test]
fn envelope_attack_decay_sustain_are_values_in_seconds() {
    let envelope = Envelope {
        attack: 0.25,
        decay: 0.5,
        sustain: 0.25,
        release: 0.125,
    };
    for (time, want) in [
        (0.0, 0.0),
        (0.125, 0.5),
        (0.25, 1.0),
        (0.5, 0.625),
        (0.75, 0.25),
        (50.0, 0.25),
    ] {
        assert!((envelope.held(time) - want).abs() < 1e-12);
    }
}

fn response(kind: FilterKind, hz: f64, rate: u32) -> f64 {
    let mut filter = Biquad::new(
        Filter {
            kind,
            hz: 1000.0,
            q: std::f32::consts::FRAC_1_SQRT_2,
        },
        rate,
    );
    let mut energy = 0.0;
    for index in 0..rate {
        let sample = filter.next((TAU * hz * f64::from(index) / f64::from(rate)).sin());
        if index >= rate / 2 {
            energy += sample * sample;
        }
    }
    (energy / f64::from(rate / 2) * 2.0).sqrt()
}

#[test]
fn filters_have_their_expected_three_frequency_responses() {
    for rate in [8_000, 44_100, 96_000] {
        for kind in [FilterKind::Low, FilterKind::High, FilterKind::Band] {
            let low = response(kind, 100.0, rate);
            let middle = response(kind, 1000.0, rate);
            let high = response(kind, 3000.0, rate);
            match kind {
                FilterKind::Low => {
                    assert!(low > 0.99);
                    assert!((middle - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.002);
                    assert!(high < 0.12);
                }
                FilterKind::High => {
                    assert!(low < 0.011);
                    assert!((middle - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.002);
                    assert!(high > 0.99);
                }
                FilterKind::Band => {
                    assert!(low < 0.15);
                    assert!((middle - 1.0).abs() < 0.002);
                    assert!(high < 0.48);
                }
            }
        }
    }
}

#[test]
fn seeded_white_noise_has_a_sample_fixture_and_brown_removes_high_energy() {
    let mut white = Generator::new(Exciter::Noise(Noise::White), 0, 48_000);
    let expected = [0.7666216164, -0.1369440059, -0.9471324568, 0.9417639563];
    for want in expected {
        assert!((white.next() - want).abs() < 1e-9);
    }
    let mut white = Generator::new(Exciter::Noise(Noise::White), 42, 48_000);
    let mut brown = Generator::new(Exciter::Noise(Noise::Brown), 42, 48_000);
    let mut previous = [0.0; 2];
    let mut differences = [0.0; 2];
    let mut energy = [0.0; 2];
    for _ in 0..48_000 {
        for (i, value) in [white.next(), brown.next()].into_iter().enumerate() {
            assert!(value.is_finite() && value.abs() <= 1.0);
            differences[i] += (value - previous[i]).powi(2);
            energy[i] += value * value;
            previous[i] = value;
        }
    }
    assert!(energy[1] > 0.1);
    assert!(differences[1] / energy[1] < differences[0] / energy[0] * 0.02);
}

#[test]
fn a_description_renders_repeatably_with_silent_edges_and_device_duration() {
    for rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
        for exciter in [
            Exciter::Noise(Noise::White),
            Exciter::Noise(Noise::Brown),
            Exciter::Oscillator {
                wave: Wave::Square,
                hz: 440.0,
            },
        ] {
            for kind in [FilterKind::Low, FilterKind::High, FilterKind::Band] {
                let mut described = sound(exciter);
                described.layers[0].filter = Some(Filter {
                    kind,
                    hz: 800.0,
                    q: 0.7,
                });
                let baked = described.bake(0.1, rate, 42).unwrap();
                assert_eq!(baked.sample_rate(), rate);
                assert_eq!(baked.samples().len(), rate as usize / 10);
                assert_eq!(
                    baked.samples(),
                    described.bake(0.1, rate, 42).unwrap().samples()
                );
                assert_eq!(baked.samples()[0], 0.0);
                assert_eq!(*baked.samples().last().unwrap(), 0.0);
                assert!(
                    baked
                        .samples()
                        .iter()
                        .all(|v| v.is_finite() && v.abs() <= 1.0)
                );
                assert!(baked.samples().iter().any(|v| v.abs() > 0.001));
            }
        }
    }
}

#[test]
fn described_sound_sample_fixture_catches_envelope_and_gain_regressions() {
    let mut described = sound(Exciter::Oscillator {
        wave: Wave::Sine,
        hz: 1000.0,
    });
    described.layers[0].envelope.attack = 0.001;
    let baked = described.bake(0.02, 8_000, 0).unwrap();
    let want = [
        0.0,
        0.03535534,
        0.1,
        0.10606602,
        0.0,
        -0.1767767,
        -0.3,
        -0.24748738,
        0.0,
    ];
    for (got, want) in baked.samples().iter().zip(want) {
        assert!((got - want).abs() < 1e-6, "{got} != {want}");
    }
}

#[test]
fn invalid_descriptions_fail_before_rendering() {
    let base = sound(Exciter::Noise(Noise::White));
    for rate in [0, 1, 7999, 192001, u32::MAX] {
        assert_eq!(base.bake(0.1, rate, 0).unwrap_err(), Error::SampleRate);
    }
    for duration in [f32::NAN, f32::INFINITY, -1.0, 0.0, 11.0] {
        assert_eq!(base.bake(duration, 48000, 0).unwrap_err(), Error::Duration);
    }
    for value in [f32::NAN, f32::INFINITY, -1.0] {
        let mut bad = base.clone();
        bad.layers[0].gain = value;
        assert_eq!(bad.bake(0.1, 48000, 0).unwrap_err(), Error::Gain);
        let mut bad = base.clone();
        bad.layers[0].envelope.release = value;
        assert_eq!(bad.bake(0.1, 48000, 0).unwrap_err(), Error::Envelope);
        let mut bad = base.clone();
        bad.layers[0].exciter = Exciter::Oscillator {
            wave: Wave::Sine,
            hz: value,
        };
        assert_eq!(bad.bake(0.1, 48000, 0).unwrap_err(), Error::Exciter);
        let mut bad = base.clone();
        bad.layers[0].filter = Some(Filter {
            kind: FilterKind::Low,
            hz: 500.0,
            q: value,
        });
        assert_eq!(bad.bake(0.1, 48000, 0).unwrap_err(), Error::Filter);
    }
    for layers in [vec![], vec![base.layers[0].clone(); MAX_LAYERS + 1]] {
        assert_eq!(
            Sound { layers }.bake(0.1, 48000, 0).unwrap_err(),
            Error::Layers
        );
    }
}

#[test]
fn a_release_longer_than_the_bake_is_rejected_instead_of_silencing_the_clip() {
    let mut described = sound(Exciter::Oscillator {
        wave: Wave::Square,
        hz: 100.0,
    });
    described.layers[0].envelope.release = 60.0;
    for duration in [1.0, MAX_BAKED_SECONDS] {
        assert_eq!(
            described.bake(duration, 48_000, 0).unwrap_err(),
            Error::Envelope
        );
    }
    // Equality is a supported full-duration release; a longer release in any later layer
    // must still be rejected, even when the first layer fits.
    described.layers[0].envelope.release = 0.1;
    let baked = described.bake(0.1, 48_000, 0).unwrap();
    assert!(baked.samples().iter().any(|sample| sample.abs() > 0.25));
    assert_eq!(*baked.samples().last().unwrap(), 0.0);
    let mut oversized = described.layers[0].clone();
    oversized.envelope.release = 0.1001;
    described.layers.push(oversized);
    assert_eq!(described.bake(0.1, 48_000, 0).unwrap_err(), Error::Envelope);
}
