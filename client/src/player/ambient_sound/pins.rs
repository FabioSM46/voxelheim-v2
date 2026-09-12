//! Every ambient bed and call, pinned bit for bit (see `audio::synth::pin`). A call's seed
//! varies per play, so each is pinned at three seeds that reach different variations.
use super::sounds::{CALLS, parrot};
use super::*;
use crate::audio::synth::pin;

const SEEDS: [u64; 3] = [0, 0x0001_0203, 0xfedc_ba98_7654_3210];

#[test]
fn every_ambient_sound_renders_bit_identical_to_its_pin() {
    let mut rendered = vec![];
    for bed in BEDS {
        // Past the half-second attack, into the sustained bed.
        rendered.push((
            format!("bed {bed:?}"),
            pin::continuous(&bed.description(), 0.75, 7),
        ));
    }
    for call in CALLS {
        for seed in SEEDS {
            let seconds = call.profile().seconds;
            rendered.push((
                format!("call {call:?} {seed:#x}"),
                pin::baked(&call.description(seed), seconds, seed),
            ));
        }
    }
    for seed in SEEDS {
        rendered.push((
            format!("parrot {seed:#x}"),
            pin::baked(&parrot(seed), 0.3, seed),
        ));
    }
    pin::assert_pins(&rendered, PINS);
}

const PINS: &[(&str, u64)] = &[
    ("bed Rain", 0x36b32a2b6064ddc1),
    ("bed DrivingRain", 0x74edeef686dd1d47),
    ("bed Snowfall", 0x231a59b864a45d7e),
    ("bed Sandstorm", 0x64c6e566bfefb451),
    ("bed Blizzard", 0x559f0e4cf0ff1163),
    ("call Rattlesnake 0x0", 0xf327365ef497b736),
    ("call Rattlesnake 0x10203", 0x3ecb19bf1dd74494),
    ("call Rattlesnake 0xfedcba9876543210", 0x69de5e7246646f97),
    ("call Crow 0x0", 0x88bf199351893ce3),
    ("call Crow 0x10203", 0x97710d9fc6817908),
    ("call Crow 0xfedcba9876543210", 0x1bf2a1ee72837e5c),
    ("call Eagle 0x0", 0x79045912484923b7),
    ("call Eagle 0x10203", 0x4bb30269e3032748),
    ("call Eagle 0xfedcba9876543210", 0x6ed7a4c043f84945),
    ("call Wolf 0x0", 0xc9565d0c05070f66),
    ("call Wolf 0x10203", 0x9b0b6ab65316ef79),
    ("call Wolf 0xfedcba9876543210", 0x05010244a30b4deb),
    ("call Cricket 0x0", 0x0f5218c29a09bb3d),
    ("call Cricket 0x10203", 0xf16735b0bc9842a5),
    ("call Cricket 0xfedcba9876543210", 0xb1616ddedfa5b1c4),
    ("parrot 0x0", 0xcd49ce621bd6e931),
    ("parrot 0x10203", 0x421da2436d465b20),
    ("parrot 0xfedcba9876543210", 0x29b73f2e85737bf5),
];
