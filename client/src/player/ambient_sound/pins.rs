//! Every ambient bed and call, pinned (see `audio::synth::pin`). A call's seed
//! varies per play, so each is pinned at three seeds that reach different variations.
use super::sounds::{CALLS, parrot};
use super::*;
use crate::audio::synth::pin;

const SEEDS: [u64; 3] = [0, 0x0001_0203, 0xfedc_ba98_7654_3210];

#[test]
fn every_ambient_sound_renders_identically_to_its_pin() {
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
            // The lane's own bake: the cricket's syllables struck apart, every other call
            // baked whole.
            let hash = pin::across_rates(|rate| {
                call.bake(seed, rate)
                    .expect("a call bakes")
                    .samples()
                    .to_vec()
            });
            rendered.push((format!("call {call:?} {seed:#x}"), hash));
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
    ("bed Rain", 0xa040685f988d1f5d),
    ("bed DrivingRain", 0xe86b8ec7715d6587),
    ("bed Snowfall", 0xed1cf911c93e1cdd),
    ("bed Sandstorm", 0xfda8ba26c0be587f),
    ("bed Blizzard", 0xc3d4de1c7dcbd314),
    ("call Rattlesnake 0x0", 0xe8557bfe9af77e0a),
    ("call Rattlesnake 0x10203", 0x43dffdb1cde851b7),
    ("call Rattlesnake 0xfedcba9876543210", 0x9474ec603879e046),
    ("call Crow 0x0", 0xb29965090082f38d),
    ("call Crow 0x10203", 0x7fefa006ff4c2bc6),
    ("call Crow 0xfedcba9876543210", 0xb494d16884b25fe4),
    ("call Eagle 0x0", 0x6cf4c30915816951),
    ("call Eagle 0x10203", 0x59229e07ce8a15a8),
    ("call Eagle 0xfedcba9876543210", 0x37a6ee76238577fb),
    ("call Wolf 0x0", 0xf6aa342c31da0018),
    ("call Wolf 0x10203", 0x47c94671194fc125),
    ("call Wolf 0xfedcba9876543210", 0xcf6002e22b04827f),
    ("call Cricket 0x0", 0x21942a4e1f8ca36c),
    ("call Cricket 0x10203", 0xe56ffa29bd6bcfd3),
    ("call Cricket 0xfedcba9876543210", 0x76002adf4923d152),
    ("parrot 0x0", 0x66e55659ffe5a331),
    ("parrot 0x10203", 0x9076fb43d629e8a8),
    ("parrot 0xfedcba9876543210", 0xc643c1321477bef0),
];
