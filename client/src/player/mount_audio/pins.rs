//! Every mount sound as its player bakes it, pinned bit for bit (see `audio::synth::pin`).
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
fn every_mount_sound_bakes_bit_identical_to_its_pin() {
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
    ("hoof Air", 0x8ed610ce00e6ef07),
    ("hoof Stone", 0x287525a66fa3d458),
    ("hoof Earth", 0xcf8ce5c5560ff8c4),
    ("hoof Sand", 0x83b4818c77fee211),
    ("hoof Wood", 0x70a8afc159bc6997),
    ("hoof Foliage", 0x1b6ea626dda60c95),
    ("hoof Glass", 0x287525a66fa3d458),
    ("hoof Water", 0x8ed610ce00e6ef07),
    ("whinny", 0xe30447a22acd6fb9),
];
