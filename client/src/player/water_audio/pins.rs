//! Every water sound as its player bakes it, pinned (see `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

/// Every cue the catalogue holds, in a fixed order so a row names one sound forever.
fn catalogue() -> Vec<(String, Cue)> {
    let mut cues = Vec::new();
    for mounted in [false, true] {
        let name = if mounted { "mounted" } else { "on foot" };
        for force in [Force::Step, Force::Fall, Force::Dive] {
            cues.push((
                format!("splash {name} {force:?}"),
                Cue::Splash { mounted, force },
            ));
        }
        cues.push((format!("stroke {name}"), Cue::Stroke { mounted }));
    }
    cues
}

#[test]
fn every_water_sound_bakes_identically_to_its_pin() {
    let rendered: Vec<_> = catalogue()
        .into_iter()
        .map(|(name, cue)| {
            let hash = pin::across_rates(|rate| {
                Waters::default()
                    .bake(cue, rate)
                    .expect("a water cue bakes")
                    .samples()
                    .to_vec()
            });
            (name, hash)
        })
        .collect();
    pin::assert_pins(&rendered, PINS);
}

#[rustfmt::skip]
const PINS: &[pin::Row] = &[
    ("splash on foot Step", 90090, [5.78283, 0.95400, 3.16200, 1.61563]),
    ("splash on foot Fall", 90090, [5.54072, 1.82099, 2.21811, 6.97823]),
    ("splash on foot Dive", 90090, [4.87372, 10.79548, 9.53169, 8.33613]),
    ("stroke on foot", 45045, [3.60542, 9.39500, 1.02463, 4.47522]),
    ("splash mounted Step", 120120, [-1.80309, 12.71356, -0.72880, -5.22680]),
    ("splash mounted Fall", 120120, [-7.49097, 27.51898, 0.81105, 0.05489]),
    ("splash mounted Dive", 120120, [-18.80713, 38.49264, -12.39352, -4.64424]),
    ("stroke mounted", 70070, [-2.69733, 8.69137, 2.24478, -0.69515]),
];
