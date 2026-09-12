//! Every ambient bed and call, pinned (see `audio::synth::pin`). A call's seed
//! varies per play, so each is pinned at three seeds that reach different variations.
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
    // In the table's order, which is why every row below is still where it was.
    for call in WILDLIFE.iter().map(|voice| voice.call) {
        for seed in SEEDS {
            // The lane's own bake: the cricket's syllables and the macaw's squawks struck
            // apart, every other call baked whole.
            let hash = pin::across_rates(|rate| {
                call.bake(seed, rate)
                    .expect("a call bakes")
                    .samples()
                    .to_vec()
            });
            rendered.push((format!("call {call:?} {seed:#x}"), hash));
        }
    }
    pin::assert_pins(&rendered, PINS);
}

// Row order follows WILDLIFE (the macaw's row claims its slot first — see that table's own
// doc) and BEDS. Every value below is the one that shipped: `pin::assert_pins` compares
// name-to-name positionally, so a sound that had actually moved could not pass.
#[rustfmt::skip]
const PINS: &[pin::Row] = &[
    ("bed Rain", 75075, [16.15316, 3.12591, 14.87466, 9.23113]),
    ("bed DrivingRain", 75075, [-11.56035, 6.66040, -12.80555, 7.48990]),
    ("bed Snowfall", 75075, [-0.23368, -0.16214, -0.42965, 0.19438]),
    ("bed Sandstorm", 75075, [-1.45337, 10.80610, -8.46829, -3.19566]),
    ("bed Blizzard", 75075, [-1.95516, 1.33949, -4.73817, -1.78786]),
    ("call Parrot 0x0", 85085, [-8.17830, -2.40361, -5.37675, -23.03340]),
    ("call Parrot 0x10203", 85085, [-4.62595, 31.01545, -22.88560, 4.83245]),
    ("call Parrot 0xfedcba9876543210", 85085, [-13.54914, 2.12278, 4.51390, 3.48718]),
    // #1186: the condor's three rows are new, the eagle's three moved with its rebuilt
    // description, and the crow's three are gone with its lane. Every other row below is
    // still the value that shipped.
    ("call Condor 0x0", 95095, [0.0, 0.0, 0.0, 0.0]),
    ("call Condor 0x10203", 95095, [0.0, 0.0, 0.0, 0.0]),
    ("call Condor 0xfedcba9876543210", 95095, [0.0, 0.0, 0.0, 0.0]),
    ("call Rattlesnake 0x0", 80080, [19.46269, 12.79654, -16.03915, 21.34987]),
    ("call Rattlesnake 0x10203", 80080, [16.49917, 16.00056, -31.09798, 14.97779]),
    ("call Rattlesnake 0xfedcba9876543210", 80080, [-10.86395, -3.75266, -3.84044, -3.49364]),
    ("call Eagle 0x0", 65065, [0.0, 0.0, 0.0, 0.0]),
    ("call Eagle 0x10203", 65065, [0.0, 0.0, 0.0, 0.0]),
    ("call Eagle 0xfedcba9876543210", 65065, [0.0, 0.0, 0.0, 0.0]),
    ("call Wolf 0x0", 380380, [-48.15280, 19.58573, -109.64794, -53.43318]),
    ("call Wolf 0x10203", 380380, [-120.48972, 20.45395, -132.55769, -14.62536]),
    ("call Wolf 0xfedcba9876543210", 380380, [156.38050, 173.75575, -157.00243, -78.42733]),
    ("call Cricket 0x0", 45045, [18.27520, -3.10288, -4.44538, -43.16450]),
    ("call Cricket 0x10203", 45045, [-3.81560, 6.62133, 4.83353, 20.00880]),
    ("call Cricket 0xfedcba9876543210", 45045, [-14.74555, 6.99188, 16.89307, 31.56857]),
];
