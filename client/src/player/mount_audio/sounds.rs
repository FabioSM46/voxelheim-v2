//! A hoof is one implement striking many grounds, so the strike is `tool_audio`'s shape:
//! a keratin knock shared by every footfall, and the ground's own response on top of it.
//! No asset files, and the classes are the palette's rather than a second opinion about
//! block ids.
use crate::{
    audio::synth::{
        Curve, Envelope, Exciter, Filter, FilterKind, Glide, Layer, Noise, Sound, Vibrato, Wave,
    },
    world::palette::MaterialClass,
};

pub(super) const HOOF_SECONDS: f32 = 0.16;
pub(super) const WHINNY_SECONDS: f32 = 1.25;

fn noise(gain: f32, attack: f32, decay: f32, hz: f32, kind: FilterKind) -> Layer {
    Layer {
        exciter: Exciter::Noise(Noise::White),
        gain,
        envelope: Envelope {
            attack,
            decay,
            sustain: 0.0,
            release: 0.015,
        },
        filter: Some(Filter { kind, hz, q: 0.7 }),
    }
}

/// A sine with the strike's soft onset: four milliseconds, never the two of a hammer.
fn tone(hz: f32, gain: f32, decay: f32) -> Layer {
    Layer {
        exciter: Exciter::Oscillator {
            wave: Wave::Sine,
            hz,
        },
        gain,
        envelope: Envelope {
            attack: 0.004,
            decay,
            sustain: 0.0,
            release: 0.015,
        },
        filter: None,
    }
}

/// White noise through a band of its own width. [`noise`]'s fixed `q` of 0.7 is so wide that
/// its skirts carry a clop's energy far above and below the middle it is centred on.
fn band(gain: f32, attack: f32, decay: f32, hz: f32, q: f32) -> Layer {
    Layer {
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz,
            q,
        }),
        ..noise(gain, attack, decay, hz, FilterKind::Band)
    }
}

pub(super) fn hoof(ground: MaterialClass) -> Sound {
    // How long the hoof's own clop rings: rock gives it nothing to sink into.
    let ring = match ground {
        MaterialClass::Stone | MaterialClass::Glass => 0.03,
        _ => 0.05,
    };
    // The hoof itself: a soft, dull clop. Two narrow bands of noise in the middle of the
    // spectrum, opened over four or five milliseconds so the onset is short without being a
    // click, over a body far too quiet and too high to thump.
    let mut layers = vec![
        band(0.55, 0.004, ring, 1000.0, 1.6),
        band(0.30, 0.005, ring * 0.8, 620.0, 1.2),
        tone(280.0, 0.05, ring * 0.7),
    ];
    layers.extend(match ground {
        // A shod hoof on rock: brighter and shorter than any other ground, with a small ring.
        MaterialClass::Stone | MaterialClass::Glass => vec![
            tone(1450.0, 0.10, 0.03),
            tone(2390.0, 0.06, 0.02),
            noise(0.16, 0.003, 0.015, 3000.0, FilterKind::High),
        ],
        // Earth and grass take the clop in: a low, short band that muffles it.
        MaterialClass::Earth => vec![band(0.35, 0.005, 0.035, 420.0, 1.0)],
        // A soft hiss of grains moving under the hoof.
        MaterialClass::Sand => vec![noise(0.35, 0.010, 0.07, 2600.0, FilterKind::Band)],
        // A hollow clop over a floor with air under it.
        MaterialClass::Wood => vec![tone(330.0, 0.16, 0.06), tone(610.0, 0.08, 0.04)],
        MaterialClass::Foliage => vec![noise(0.30, 0.008, 0.09, 1800.0, FilterKind::High)],
        // Neither is solid, so the ground probe never answers them. Total over the enum all
        // the same, so a later class cannot silently take the wrong arm.
        MaterialClass::Air | MaterialClass::Water => vec![band(0.20, 0.010, 0.07, 700.0, 0.7)],
    });
    Sound { layers }
}

/// Where the call starts and ends, in hertz, and how long its fall takes.
const SQUEAL_HZ: f32 = 1250.0;
const CLOSE_HZ: f32 = 470.0;
const FALL_SECONDS: f32 = 1.0;
/// The flutter: eleven wavers a second, five percent either side of the falling pitch,
/// opening over the first third of a second so the onset is a clean squeal.
const FLUTTER: Vibrato = Vibrato {
    hz: 11.0,
    depth: 0.05,
    onset: 0.3,
};

/// The one pitch track every voiced layer follows, at a harmonic of it. Every harmonic has
/// the same curve and the same fractional vibrato, so the partials stay locked together as
/// one voice rather than beating against each other.
fn pitch(harmonic: f32, wave: Wave) -> Glide {
    Glide {
        wave,
        from: SQUEAL_HZ * harmonic,
        to: CLOSE_HZ * harmonic,
        seconds: FALL_SECONDS,
        curve: Curve::Exponential,
        vibrato: FLUTTER,
    }
}

/// A voiced layer: a partial of the pitch track shaped by one formant band of the head.
fn formant(glide: Glide, gain: f32, hz: f32, q: f32, envelope: (f32, f32, f32)) -> Layer {
    let (attack, decay, sustain) = envelope;
    Layer {
        exciter: Exciter::Glide(glide),
        gain,
        envelope: Envelope {
            attack,
            decay,
            sustain,
            release: 0.03,
        },
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz,
            q,
        }),
    }
}

/// Breath: noise that swells in over most of the call and closes it.
fn breath(kind: Noise, gain: f32, filter: Filter, attack: f32, decay: f32) -> Layer {
    Layer {
        exciter: Exciter::Noise(kind),
        gain,
        envelope: Envelope {
            attack,
            decay,
            sustain: 0.0,
            release: 0.03,
        },
        filter: Some(filter),
    }
}

pub(super) fn whinny() -> Sound {
    // One falling, fluttering pitch through the formants of a long head: the nasal band high
    // at 2.8 kHz carries the squeal and is gone within half a second; 1.4 kHz is the body of
    // the call; 700 Hz grows as the pitch falls into it, and a breath takes over at the end.
    //
    // A voice is not a note, so each band is textured rather than a clean partial. At the nose,
    // a saw's dense harmonics are picked out by a narrow band: a rasp that moves as the pitch
    // walks its harmonics across the band, where one sine would whistle. In the body, a saw
    // three and a half percent above the voice beats against it at 44 Hz falling to 16 Hz —
    // the roughness of a throat, not a second note. Through both, noise shaped by the same
    // bands is the air the voice is made of.
    //
    // The close's breath is low-passed rather than banded: a band's skirts fall only 6 dB an
    // octave, and on white noise they would carry the close brighter than the squeal.
    let band = |hz, q| Filter {
        kind: FilterKind::Band,
        hz,
        q,
    };
    let low = |hz| Filter {
        kind: FilterKind::Low,
        hz,
        q: 0.7,
    };
    Sound {
        layers: vec![
            // The nose.
            formant(pitch(1.0, Wave::Saw), 0.10, 2800.0, 2.0, (0.02, 0.45, 0.0)),
            formant(pitch(2.0, Wave::Sine), 0.12, 2800.0, 1.4, (0.02, 0.40, 0.0)),
            breath(Noise::White, 0.12, band(2800.0, 3.0), 0.02, 0.45),
            // The body, and its roughness.
            formant(
                pitch(1.0, Wave::Triangle),
                0.26,
                1400.0,
                1.1,
                (0.04, 1.10, 0.05),
            ),
            formant(
                pitch(1.035, Wave::Saw),
                0.07,
                1400.0,
                1.1,
                (0.04, 1.00, 0.05),
            ),
            breath(Noise::White, 0.20, band(1400.0, 3.0), 0.04, 1.05),
            // The chest the pitch falls into.
            formant(
                pitch(1.0, Wave::Triangle),
                0.20,
                700.0,
                0.9,
                (0.15, 1.00, 0.30),
            ),
            // The breath that closes it.
            breath(Noise::White, 0.35, low(900.0), 0.85, 0.40),
            breath(Noise::Brown, 1.0, low(500.0), 0.90, 0.30),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::MiningTool;
    use crate::player::tool_audio::sounds::{STRIKE_SECONDS, strike};
    use crate::world::palette::{self, DIRT, LEAVES, PLANKS, SAND, STONE};

    const GROUNDS: [MaterialClass; 8] = [
        MaterialClass::Air,
        MaterialClass::Stone,
        MaterialClass::Earth,
        MaterialClass::Sand,
        MaterialClass::Wood,
        MaterialClass::Foliage,
        MaterialClass::Glass,
        MaterialClass::Water,
    ];
    const RATE: u32 = 48_000;

    /// Energy per DFT bin from 0 Hz to Nyquist, one bin per `1 / duration` hertz. Goertzel's
    /// recurrence per bin rather than an FFT: no crate, and the length need not be a power of
    /// two. Every baked sound starts and ends at exact silence, so the rectangular window
    /// leaks nothing a Hann window would have saved.
    fn spectrum(samples: &[f32]) -> Vec<(f32, f64)> {
        spectrum_every(samples, 1)
    }
    /// [`spectrum`] at every `stride`th bin only: a long sound's shares, at a fraction of the
    /// arithmetic, from bins still far narrower than any band a share is read over.
    fn spectrum_every(samples: &[f32], stride: usize) -> Vec<(f32, f64)> {
        let n = samples.len();
        (0..n / 2)
            .step_by(stride)
            .map(|bin| {
                let omega = std::f64::consts::TAU * bin as f64 / n as f64;
                let coefficient = 2.0 * omega.cos();
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                for &x in samples {
                    let s0 = f64::from(x) + coefficient * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                let power = s1 * s1 + s2 * s2 - coefficient * s1 * s2;
                (bin as f32 * RATE as f32 / n as f32, power)
            })
            .collect()
    }
    /// The spectrum's centre of mass, in hertz: where a strike's energy sits on average.
    fn centroid(spectrum: &[(f32, f64)]) -> f32 {
        let total: f64 = spectrum.iter().map(|(_, power)| power).sum();
        (spectrum
            .iter()
            .map(|(hz, power)| f64::from(*hz) * power)
            .sum::<f64>()
            / total) as f32
    }
    /// The fraction of a strike's energy below `hz`.
    fn share_below(spectrum: &[(f32, f64)], hz: f32) -> f32 {
        let total: f64 = spectrum.iter().map(|(_, power)| power).sum();
        (spectrum
            .iter()
            .filter(|(bin, _)| *bin < hz)
            .map(|(_, power)| power)
            .sum::<f64>()
            / total) as f32
    }
    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |peak, x| x.abs().max(peak))
    }

    /// When a strike's energy arrives on average, in seconds from its start.
    fn mean_time(samples: &[f32]) -> f32 {
        samples
            .iter()
            .enumerate()
            .map(|(index, x)| index as f32 * x * x)
            .sum::<f32>()
            / energy(samples)
            / RATE as f32
    }

    /// #1161: every footfall was a bass-drum thump. A 140 Hz knock and noise low-passed at
    /// 900 Hz on every ground, and on earth 300 Hz noise and a 70 Hz sine on top: #1143's
    /// strike measured a 111 Hz centroid on earth with 97% of its energy under 200 Hz, and
    /// 35-93% on every other ground. Now the darkest ground, wood, sits at 666 Hz and no
    /// ground puts more than 0.4% under 200 Hz.
    #[test]
    fn every_ground_strikes_a_soft_clop_in_the_middle_of_the_spectrum_rather_than_a_drum() {
        for ground in GROUNDS {
            let sound = hoof(ground);
            for layer in &sound.layers {
                // No hard onset anywhere in the strike, and no sine low enough to thump.
                assert!(layer.envelope.attack >= 0.003, "{ground:?}: {layer:?}");
                if let Exciter::Oscillator { hz, .. } = layer.exciter {
                    assert!(hz >= 200.0, "{ground:?}: a {hz} Hz knock");
                }
            }
            let spectrum = spectrum(&samples(sound, HOOF_SECONDS));
            let centroid = centroid(&spectrum);
            let low = share_below(&spectrum, 200.0);
            assert!(centroid > 500.0, "{ground:?}: centroid at {centroid} Hz");
            assert!(low < 0.02, "{ground:?}: {low} of the energy under 200 Hz");
        }
    }

    /// Softer than an implement striking the same ground — the quietest of the four, so the
    /// hoof is never the loudest thing a player at work hears — and still heard: the energy
    /// above 200 Hz, where an ear and a laptop speaker both are, measures 6.4 on earth and
    /// more on every other ground.
    #[test]
    fn a_hoof_is_heard_but_strikes_softer_than_any_tool_on_the_same_ground() {
        for ground in GROUNDS {
            let hoof = samples(hoof(ground), HOOF_SECONDS);
            let softest = [
                MiningTool::Hand,
                MiningTool::Shovel,
                MiningTool::Pickaxe,
                MiningTool::Axe,
            ]
            .map(|tool| peak(&samples(strike(tool, ground), STRIKE_SECONDS)))
            .into_iter()
            .fold(f32::INFINITY, f32::min);
            assert!(
                peak(&hoof) < softest * 0.8,
                "{ground:?}: a hoof peaks at {}, the softest tool at {softest}",
                peak(&hoof)
            );
            let audible = energy(&hoof) * (1.0 - share_below(&spectrum(&hoof), 200.0));
            assert!(audible > 4.0, "{ground:?}: {audible} above 200 Hz");
        }
    }

    #[test]
    fn rock_rings_brighter_and_shorter_than_earth() {
        let stone = samples(hoof(MaterialClass::Stone), HOOF_SECONDS);
        let earth = samples(hoof(MaterialClass::Earth), HOOF_SECONDS);
        assert!(centroid(&spectrum(&stone)) > centroid(&spectrum(&earth)) * 2.0);
        assert!(
            mean_time(&stone) < mean_time(&earth),
            "stone {} s, earth {} s",
            mean_time(&stone),
            mean_time(&earth)
        );
    }

    fn samples(sound: Sound, seconds: f32) -> Vec<f32> {
        sound.bake(seconds, 48_000, 7).unwrap().samples().to_vec()
    }
    fn difference(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum::<f32>()
    }
    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|x| x * x).sum()
    }

    #[test]
    fn five_canonical_grounds_strike_distinctly() {
        let strikes: Vec<_> = [STONE, DIRT, PLANKS, LEAVES, SAND]
            .into_iter()
            .map(|id| samples(hoof(palette::material_class(id)), HOOF_SECONDS))
            .collect();
        for (i, a) in strikes.iter().enumerate() {
            for b in strikes.iter().skip(i + 1) {
                assert!(difference(a, b) > 1.0);
            }
        }
    }

    #[test]
    fn a_whinny_outlasts_a_strike_and_does_not_open_like_one() {
        let whinny = samples(whinny(), WHINNY_SECONDS);
        let strike = samples(hoof(MaterialClass::Earth), HOOF_SECONDS);
        // Past the end of any strike, the call is still sounding.
        assert!(energy(&whinny[strike.len() * 3..]) > 1.0);
        assert!(difference(&whinny[..strike.len()], &strike) > 1.0);
    }

    /// The whinny's layers of one kind baked alone: the voiced partials, or the breath. A bake
    /// is a sum of its layers, so the two halves can be read apart.
    fn part(voiced: bool) -> Vec<f32> {
        let layers = whinny()
            .layers
            .into_iter()
            .filter(|layer| matches!(layer.exciter, Exciter::Glide(_)) == voiced)
            .collect();
        samples(Sound { layers }, WHINNY_SECONDS)
    }

    /// The pitch a voiced passage is at, every ten milliseconds, from a thirty-millisecond
    /// window: the shortest lag between 300 and 1800 Hz whose normalised autocorrelation comes
    /// within a tenth of the best, placed between samples by a parabola through its two
    /// neighbours. The shortest such lag rather than the best, so a period twice as long never
    /// reads as an octave's fall. Windows under a fifth of the loudest one's amplitude are
    /// skipped: they have no pitch worth reading.
    fn pitch_track(samples: &[f32]) -> Vec<(f32, f32)> {
        const WINDOW: usize = 1440;
        const HOP: usize = 480;
        let (shortest, longest) = (RATE as usize / 1800, RATE as usize / 300);
        let starts = (0..samples.len() - WINDOW - longest - 1).step_by(HOP);
        let loudest = starts
            .clone()
            .map(|start| energy(&samples[start..start + WINDOW]))
            .fold(0.0, f32::max);
        starts
            .filter_map(|start| {
                let window = &samples[start..start + WINDOW];
                if energy(window) < loudest * 0.04 {
                    return None;
                }
                let correlation = |lag: usize| {
                    let later = &samples[start + lag..start + lag + WINDOW];
                    let (mut cross, mut here, mut there) = (0.0f64, 0.0f64, 0.0f64);
                    for (x, y) in window.iter().zip(later) {
                        let (x, y) = (f64::from(*x), f64::from(*y));
                        cross += x * y;
                        here += x * x;
                        there += y * y;
                    }
                    cross / (here * there).sqrt()
                };
                let r: Vec<f64> = (0..=longest + 1)
                    .map(|lag| {
                        if lag + 1 < shortest {
                            0.0
                        } else {
                            correlation(lag)
                        }
                    })
                    .collect();
                let best = r[shortest..=longest]
                    .iter()
                    .copied()
                    .fold(f64::MIN, f64::max);
                let lag = (shortest..=longest).find(|&lag| {
                    r[lag] >= best * 0.9 && r[lag] >= r[lag - 1] && r[lag] >= r[lag + 1]
                })?;
                let (before, at, after) = (r[lag - 1], r[lag], r[lag + 1]);
                let offset = 0.5 * (before - after) / (before - 2.0 * at + after);
                Some((
                    (start + WINDOW / 2) as f32 / RATE as f32,
                    (f64::from(RATE) / (lag as f64 + offset)) as f32,
                ))
            })
            .collect()
    }

    /// Each pitch divided by the mean of the nine around it — ninety milliseconds, one full
    /// waver of the flutter — minus one: the flutter alone, with the fall taken out.
    fn trend(track: &[(f32, f32)]) -> Vec<f32> {
        (0..track.len())
            .map(|index| {
                let around = &track[index.saturating_sub(4)..(index + 5).min(track.len())];
                around.iter().map(|(_, hz)| hz).sum::<f32>() / around.len() as f32
            })
            .collect()
    }

    /// #1160: the call was four fixed pitches — 1180, 1010, 840 and 640 Hz — each a jump of
    /// 14 to 24% from the one before, which the ear hears as stairs. It now falls as one
    /// continuous track, and no ten milliseconds of it moves by more than 8%: the fall and
    /// the flutter together.
    #[test]
    fn the_whinny_falls_continuously_from_a_high_squeal_to_a_low_close() {
        let track = pitch_track(&part(true));
        assert!(track.len() > 80, "{} voiced windows", track.len());
        let (start, end) = (track[0].1, track[track.len() - 1].1);
        assert!(start > 1100.0, "opens at {start} Hz");
        assert!(end < 560.0, "closes at {end} Hz");
        for pair in track.windows(2) {
            let step = (pair[1].1 / pair[0].1 - 1.0).abs();
            assert!(step < 0.08, "a {step} step: {pair:?}");
        }
        // With the flutter averaged out, the pitch only ever falls or holds: within 1%, which
        // is what a ninety-millisecond mean leaves of the flutter once the fall has ended.
        for pair in trend(&track).windows(2) {
            assert!(pair[1] < pair[0] * 1.01, "the fall turns back up: {pair:?}");
        }
    }

    #[test]
    fn the_whinny_flutters_fast_through_its_middle() {
        let track = pitch_track(&part(true));
        let flutter: Vec<f32> = track
            .iter()
            .zip(trend(&track))
            .filter(|((time, _), _)| (0.3..1.0).contains(time))
            .map(|((_, hz), trend)| hz / trend - 1.0)
            .collect();
        let crossings = flutter
            .windows(2)
            .filter(|pair| pair[0].signum() != pair[1].signum())
            .count();
        let rate = crossings as f32 / 2.0 / (flutter.len() as f32 * 0.01);
        assert!((8.0..=16.0).contains(&rate), "flutter at {rate} Hz");
        let depth = (flutter.iter().map(|x| x * x).sum::<f32>() / flutter.len() as f32).sqrt();
        assert!(depth > 0.015, "flutter {depth} deep");
    }

    #[test]
    fn the_whinny_opens_bright_and_closes_low_and_breathy_below_four_kilohertz() {
        let call = samples(whinny(), WHINNY_SECONDS);
        let above = 1.0 - share_below(&spectrum_every(&call, 16), 4000.0);
        assert!(above < 0.05, "{above} of the call above 4 kHz");
        let span = RATE as usize * 3 / 10;
        let open = centroid(&spectrum(&call[..span]));
        let close = centroid(&spectrum(&call[call.len() - span..]));
        assert!(
            open > close * 1.5,
            "opens at {open} Hz, closes at {close} Hz"
        );
        let (voiced, breath) = (part(true), part(false));
        let breathy = |from: f32, to: f32| {
            let range = (from * RATE as f32) as usize..(to * RATE as f32) as usize;
            let breath = energy(&breath[range.clone()]);
            breath / (breath + energy(&voiced[range]))
        };
        assert!(
            breathy(0.95, 1.25) > breathy(0.3, 0.7) * 2.0,
            "breath is {} of the close and {} of the middle",
            breathy(0.95, 1.25),
            breathy(0.3, 0.7)
        );
    }

    /// #1143's call peaked at 0.67 with an RMS of 0.105 at 48 kHz. The voice that replaces it
    /// keeps that level, never clips, and starts and ends at silence at every device rate.
    #[test]
    fn the_whinny_keeps_the_level_of_the_call_it_replaces_at_every_rate() {
        for rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
            let baked = whinny().bake(WHINNY_SECONDS, rate, 1122).unwrap();
            let call = baked.samples();
            let rms = (energy(call) / call.len() as f32).sqrt();
            assert!(peak(call) < 0.85, "{rate}: peaks at {}", peak(call));
            assert!((0.07..0.15).contains(&rms), "{rate}: RMS {rms}");
            assert_eq!(call.first(), Some(&0.0));
            assert_eq!(call.last(), Some(&0.0));
        }
    }

    /// A voice is textured, never a note: the call is one pitch track — every voiced layer the
    /// same fall and the same flutter — made rough by a saw's harmonics through a formant, and
    /// airy by noise through the formants as well. Clean partials alone would whistle.
    #[test]
    fn the_whinny_is_one_textured_pitch_track_through_formants() {
        let layers = whinny().layers;
        let aspirated = layers.iter().any(|layer| {
            matches!(layer.exciter, Exciter::Noise(_))
                && matches!(
                    layer.filter,
                    Some(Filter { kind: FilterKind::Band, hz, .. }) if (1200.0..=3200.0).contains(&hz)
                )
        });
        assert!(aspirated, "no air through the formants");
        let voiced: Vec<_> = layers
            .into_iter()
            .filter_map(|layer| match layer.exciter {
                Exciter::Glide(glide) => Some((glide, layer.filter)),
                Exciter::Oscillator { .. } => panic!("a fixed pitch in the call: {layer:?}"),
                Exciter::Noise(_) => None,
            })
            .collect();
        assert!(voiced.len() >= 3);
        assert!(
            voiced.iter().any(|(glide, _)| glide.wave == Wave::Saw),
            "no rasp: every partial is a clean waveform"
        );
        let (first, _) = voiced[0];
        let mut formants = vec![];
        for (glide, filter) in voiced {
            // Every partial rides one track: the same fall, the same flutter.
            assert!((glide.to / glide.from - first.to / first.from).abs() < 1e-6);
            assert_eq!(
                (glide.seconds, glide.curve, glide.vibrato),
                (first.seconds, first.curve, first.vibrato)
            );
            let Some(Filter {
                kind: FilterKind::Band,
                hz,
                ..
            }) = filter
            else {
                panic!("a partial outside a formant: {filter:?}");
            };
            formants.push(hz);
        }
        assert!(formants.iter().any(|hz| (600.0..=800.0).contains(hz)));
        assert!(formants.iter().any(|hz| (1200.0..=1600.0).contains(hz)));
        assert!(formants.iter().any(|hz| (2400.0..=3200.0).contains(hz)));
    }

    #[test]
    fn catalogue_is_bounded_at_every_supported_device_rate() {
        for rate in [8000, 44100, 48000, 192000] {
            let mut sounds = vec![(whinny(), WHINNY_SECONDS)];
            for ground in [
                MaterialClass::Air,
                MaterialClass::Stone,
                MaterialClass::Earth,
                MaterialClass::Sand,
                MaterialClass::Wood,
                MaterialClass::Foliage,
                MaterialClass::Glass,
                MaterialClass::Water,
            ] {
                sounds.push((hoof(ground), HOOF_SECONDS));
            }
            for (sound, seconds) in sounds {
                let baked = sound.bake(seconds, rate, 17).unwrap();
                assert!(
                    baked
                        .samples()
                        .iter()
                        .all(|x| x.is_finite() && x.abs() <= 1.0)
                );
                assert_eq!(baked.samples().first(), Some(&0.0));
                assert_eq!(baked.samples().last(), Some(&0.0));
            }
        }
    }
}
