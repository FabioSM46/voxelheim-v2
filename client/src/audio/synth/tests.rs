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
            gate: None,
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

/// #1184: a gate is the layer's own amplitude struck open and shut. The whole of its effect is
/// a multiplication of the layer it sits on, so the same description baked with and without one
/// differs by exactly the gate's shape — full where it opens, falling linearly across the duty,
/// and **exact** zero for the rest of every period, because the gate is applied after the
/// filter and no resonant tail may smear a gap into a dip.
///
/// 125 openings a second at 8 kHz is a period of exactly 64 samples, and a quarter duty opens
/// for exactly 16 of them: a rate whose phase step is a power of two, so this test's arithmetic
/// is the renderer's and not an approximation of it.
#[test]
fn a_gate_strikes_its_layer_open_and_shut_between_exact_silences() {
    let (rate, duty) = (8_000, 0.25);
    let mut described = sound(Exciter::Noise(Noise::White));
    described.layers[0].envelope = Envelope {
        attack: 0.001,
        decay: 0.0,
        sustain: 1.0,
        release: 0.001,
    };
    let open = described.bake(0.5, rate, 9).unwrap();
    described.layers[0].gate = Some(Gate {
        from: 125.0,
        to: 125.0,
        seconds: 0.1,
        curve: Curve::Linear,
        duty,
    });
    let struck = described.bake(0.5, rate, 9).unwrap();
    let mut silent = 0usize;
    for (index, (sounded, gated)) in open.samples().iter().zip(struck.samples()).enumerate() {
        let phase = (index % 64) as f32 / 64.0;
        let want = if phase < duty {
            1.0 - phase / duty
        } else {
            silent += 1;
            0.0
        };
        assert!(
            (gated - sounded * want).abs() < 1e-7,
            "sample {index}: {gated} is not {sounded} times {want}"
        );
    }
    // Three quarters of every period is silence, and it is exact rather than quiet: the layer
    // it gates is noise, which is never exactly zero of its own accord.
    let count = struck.samples().len();
    assert!(silent.abs_diff(count * 3 / 4) < 64, "{silent} of {count}");
    assert!(struck.samples().iter().filter(|v| **v == 0.0).count() >= silent);
    assert!(open.samples().iter().filter(|v| **v == 0.0).count() < 8);

    // A gate whose rate climbs strikes more often at the end than at the start.
    described.layers[0].gate = Some(Gate {
        from: 50.0,
        to: 390.0,
        seconds: 0.5,
        curve: Curve::Linear,
        duty,
    });
    let winding = described.bake(0.5, rate, 9).unwrap();
    let openings = |half: &[f32]| {
        half.windows(2)
            .filter(|pair| pair[0] == 0.0 && pair[1] != 0.0)
            .count()
    };
    let samples = winding.samples();
    let (first, last) = samples.split_at(samples.len() / 2);
    assert!(
        openings(last) > openings(first) * 2,
        "{} openings then {}",
        openings(first),
        openings(last)
    );
}

/// Every bound a gate has, refused before anything is allocated, the way every other field is.
/// A duty of one is refused with the rest: a gate that never closes is a gain, and a gain is
/// what the layer already has.
#[test]
fn a_gate_outside_its_bounds_is_refused_before_rendering() {
    let good = Gate {
        from: 60.0,
        to: 90.0,
        seconds: 0.3,
        curve: Curve::Exponential,
        duty: 0.2,
    };
    let refused = |gate: Gate| {
        let mut described = sound(Exciter::Noise(Noise::White));
        described.layers[0].gate = Some(gate);
        described.bake(0.5, 8_000, 0)
    };
    assert!(refused(good).is_ok());
    // A twentieth of the sample rate, so the shortest opening still holds a sample of its own.
    let limit = 8_000.0 * MAX_GATE_FRACTION;
    assert!((limit - 400.0).abs() < f32::EPSILON);
    for bad in [
        Gate {
            from: f32::NAN,
            ..good
        },
        Gate { from: 0.0, ..good },
        Gate {
            to: f32::INFINITY,
            ..good
        },
        Gate {
            to: limit + 1.0,
            ..good
        },
        Gate {
            seconds: 0.0,
            ..good
        },
        Gate {
            seconds: 61.0,
            ..good
        },
        Gate { duty: 0.0, ..good },
        Gate { duty: 0.04, ..good },
        Gate { duty: 1.0, ..good },
        Gate {
            duty: f32::NAN,
            ..good
        },
    ] {
        assert_eq!(refused(bad).unwrap_err(), Error::Gate, "{bad:?}");
    }
    // Both ends obey the bound, and the bound follows the rate: what 8 kHz refuses, 48 kHz
    // renders.
    let mut fast = sound(Exciter::Noise(Noise::White));
    fast.layers[0].gate = Some(Gate {
        from: limit + 1.0,
        to: limit + 1.0,
        ..good
    });
    assert_eq!(fast.bake(0.5, 8_000, 0).unwrap_err(), Error::Gate);
    assert!(fast.bake(0.5, 48_000, 0).is_ok());
}

fn glide(from: f32, to: f32, curve: Curve, vibrato: Vibrato) -> Glide {
    Glide {
        wave: Wave::Sine,
        from,
        to,
        seconds: 0.8,
        curve,
        vibrato,
    }
}

/// The frequency a rendered sine is at, once per cycle: the reciprocal of the time between
/// two rising zero crossings, each placed between its two samples by linear interpolation,
/// and stamped at the middle of that cycle.
fn frequency_track(glide: Glide, rate: u32, seconds: f64) -> Vec<(f64, f64)> {
    let mut generator = Generator::new(Exciter::Glide(glide), 0, rate);
    let count = (seconds * f64::from(rate)) as usize;
    let samples: Vec<f64> = (0..count).map(|_| generator.next()).collect();
    let crossings: Vec<f64> = samples
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0] < 0.0 && pair[1] >= 0.0)
        .map(|(index, pair)| (index as f64 + pair[0] / (pair[0] - pair[1])) / f64::from(rate))
        .collect();
    crossings
        .windows(2)
        .map(|pair| ((pair[0] + pair[1]) / 2.0, 1.0 / (pair[1] - pair[0])))
        .collect()
}

#[test]
fn a_glide_travels_its_curve_monotonically_and_holds_where_it_ends() {
    for rate in [8_000, 44_100, 48_000, 96_000] {
        for curve in [Curve::Linear, Curve::Exponential] {
            for (from, to) in [(300.0, 1200.0), (1200.0, 300.0)] {
                let glide = glide(from, to, curve, Vibrato::NONE);
                let track = frequency_track(glide, rate, 1.0);
                let rising = to > from;
                for pair in track.windows(2) {
                    let (earlier, later) = (pair[0].1, pair[1].1);
                    // Monotonic within one percent: at 8 kHz a 1200 Hz cycle is under seven
                    // samples, and interpolating its crossings measures 0.5% of noise.
                    if rising {
                        assert!(later > earlier * 0.99, "{rate} {curve:?}: {pair:?}");
                    } else {
                        assert!(later < earlier * 1.01, "{rate} {curve:?}: {pair:?}");
                    }
                }
                for (time, hz) in &track {
                    let want = glide.hz_at(*time);
                    assert!(
                        (hz - want).abs() < want * 0.015,
                        "{rate} {curve:?} {from}->{to}: {hz} Hz at {time} s, want {want}"
                    );
                }
                let (first, last) = (track[0].1, track[track.len() - 1].1);
                assert!((first - f64::from(from)).abs() < f64::from(from) * 0.02);
                assert!((last - f64::from(to)).abs() < f64::from(to) * 0.01);
            }
        }
        // The two curves part in the middle: 750 Hz and the geometric mean, 600 Hz.
        let linear = glide(300.0, 1200.0, Curve::Linear, Vibrato::NONE);
        let exponential = glide(300.0, 1200.0, Curve::Exponential, Vibrato::NONE);
        // Within the f32 the duration is written in.
        assert!((linear.hz_at(0.4) - 750.0).abs() < 1e-3);
        assert!((exponential.hz_at(0.4) - 600.0).abs() < 1e-3);
    }
}

#[test]
fn a_vibrato_swings_at_its_rate_and_depth_and_opens_over_its_onset() {
    for rate in [44_100, 48_000] {
        let vibrato = Vibrato {
            hz: 6.0,
            depth: 0.04,
            onset: 0.0,
        };
        let track = frequency_track(glide(1000.0, 1000.0, Curve::Linear, vibrato), rate, 1.0);
        let highest = track.iter().map(|(_, hz)| *hz).fold(0.0, f64::max);
        let lowest = track.iter().map(|(_, hz)| *hz).fold(f64::MAX, f64::min);
        assert!((highest - 1040.0).abs() < 4.0, "{rate}: highest {highest}");
        assert!((lowest - 960.0).abs() < 4.0, "{rate}: lowest {lowest}");
        let swings = track
            .windows(2)
            .filter(|pair| (pair[0].1 - 1000.0).signum() != (pair[1].1 - 1000.0).signum())
            .count();
        // Six cycles a second cross the centre twelve times.
        assert!((11..=13).contains(&swings), "{rate}: {swings} crossings");

        let opening = Vibrato {
            onset: 0.5,
            ..vibrato
        };
        let track = frequency_track(glide(1000.0, 1000.0, Curve::Linear, opening), rate, 1.0);
        let widest = |from: f64, to: f64| {
            track
                .iter()
                .filter(|(time, _)| (from..to).contains(time))
                .map(|(_, hz)| (hz - 1000.0).abs())
                .fold(0.0, f64::max)
        };
        assert!(widest(0.0, 0.1) < 10.0, "{rate}: {}", widest(0.0, 0.1));
        assert!(widest(0.5, 1.0) > 36.0, "{rate}: {}", widest(0.5, 1.0));
    }
}

#[test]
fn a_glide_never_steps_and_a_steady_one_is_the_oscillator_bit_for_bit() {
    let fluttering = Vibrato {
        hz: 12.0,
        depth: 0.08,
        onset: 0.1,
    };
    for rate in [8_000, 44_100, 48_000, 192_000] {
        let falling = glide(1500.0, 400.0, Curve::Exponential, fluttering);
        let mut generator = Generator::new(Exciter::Glide(falling), 0, rate);
        let bound = TAU * f64::from(falling.peak()) / f64::from(rate) + 1e-9;
        let mut previous = generator.next();
        for _ in 1..rate {
            let sample = generator.next();
            // A sine moves by no more than its phase step, which is its frequency's.
            assert!((sample - previous).abs() <= bound, "{rate}");
            previous = sample;
        }
        for wave in [Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle] {
            let steady = Glide {
                wave,
                ..glide(440.0, 440.0, Curve::Exponential, Vibrato::NONE)
            };
            let mut glide = Generator::new(Exciter::Glide(steady), 0, rate);
            let mut oscillator = Generator::new(Exciter::Oscillator { wave, hz: 440.0 }, 0, rate);
            for _ in 0..rate / 10 {
                assert_eq!(glide.next().to_bits(), oscillator.next().to_bits());
            }
        }
        let baked = Sound {
            layers: vec![Layer {
                exciter: Exciter::Glide(falling),
                ..sound(Exciter::Noise(Noise::White)).layers[0].clone()
            }],
        }
        .bake(1.0, rate, 0)
        .unwrap();
        assert_eq!(baked.samples()[0], 0.0);
        assert_eq!(*baked.samples().last().unwrap(), 0.0);
        assert!(
            baked
                .samples()
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 1.0)
        );
    }
}

#[test]
fn a_glide_outside_the_oscillator_bounds_is_refused_before_rendering() {
    let good = glide(
        800.0,
        300.0,
        Curve::Linear,
        Vibrato {
            hz: 10.0,
            depth: 0.05,
            onset: 0.2,
        },
    );
    let refused = |glide: Glide| {
        let mut described = sound(Exciter::Glide(glide));
        described.layers[0].envelope.release = 0.01;
        described.bake(0.1, 8_000, 0)
    };
    assert!(refused(good).is_ok());
    let limit = 8_000.0 * 0.45;
    for bad in [
        Glide {
            from: f32::NAN,
            ..good
        },
        Glide { to: 0.0, ..good },
        Glide {
            to: limit + 1.0,
            ..good
        },
        Glide {
            seconds: 0.0,
            ..good
        },
        Glide {
            seconds: f32::INFINITY,
            ..good
        },
        // Each end is inside the bound, and the vibrato carries the peak over it.
        Glide {
            from: limit,
            ..good
        },
        Glide {
            vibrato: Vibrato {
                depth: 0.6,
                ..good.vibrato
            },
            ..good
        },
        Glide {
            vibrato: Vibrato {
                hz: -1.0,
                ..good.vibrato
            },
            ..good
        },
        Glide {
            vibrato: Vibrato {
                onset: f32::NAN,
                ..good.vibrato
            },
            ..good
        },
    ] {
        assert_eq!(refused(bad).unwrap_err(), Error::Exciter, "{bad:?}");
    }
}
