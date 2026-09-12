//! Every mount sound as its player bakes it, pinned (see `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

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

#[test]
fn every_mount_sound_bakes_identically_to_its_pin() {
    let cues = GROUNDS
        .into_iter()
        .map(|ground| (format!("hoof {ground:?}"), Cue::Hoof(ground)))
        .chain([("whinny".to_string(), Cue::Whinny)]);
    let rendered: Vec<_> = cues
        .map(|(name, cue)| {
            let hash = pin::across_rates(|rate| {
                Mounts::default()
                    .bake(cue, rate)
                    .expect("a mount cue bakes")
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
    ("hoof Air", 16016, [-0.36095, 3.51984, 0.77844, 2.50563]),
    ("hoof Stone", 16016, [6.86085, 7.85808, 4.46710, -2.89134]),
    ("hoof Earth", 16016, [2.07080, 4.01297, -0.32366, 3.09828]),
    ("hoof Sand", 16016, [-5.23344, 7.32789, 4.03183, 8.41880]),
    ("hoof Wood", 16016, [-2.79734, 3.53200, -3.22070, -3.16936]),
    ("hoof Foliage", 16016, [7.71461, 2.02446, 1.89802, 2.50821]),
    ("hoof Glass", 16016, [6.86085, 7.85808, 4.46710, -2.89134]),
    ("hoof Water", 16016, [-0.36095, 3.51984, 0.77844, 2.50563]),
    ("whinny", 125125, [-74.40801, -57.63112, -14.82609, -34.06357]),
];
