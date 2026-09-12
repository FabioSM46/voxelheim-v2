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

const PINS: &[(&str, u64)] = &[
    ("hoof Air", 0xc15a50d47ba2505b),
    ("hoof Stone", 0xa41c116595b1cd5e),
    ("hoof Earth", 0xf18e4eeb856a66e3),
    ("hoof Sand", 0x735c8ae98c2461df),
    ("hoof Wood", 0xdfd3401f58717584),
    ("hoof Foliage", 0xa3d35e444a19e25c),
    ("hoof Glass", 0xa41c116595b1cd5e),
    ("hoof Water", 0xc15a50d47ba2505b),
    ("whinny", 0x5f7ea41c19d14724),
];
