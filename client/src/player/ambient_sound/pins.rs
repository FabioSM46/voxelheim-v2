//! Every ambient bed and call, pinned (see `audio::synth::pin`). A call's seed
//! varies per play, so each is pinned at three seeds that reach different variations.
use super::sounds::{CALLS, Call};
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
    for call in CALLS.into_iter().chain([Call::Parrot]) {
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

#[rustfmt::skip]
const PINS: &[pin::Row] = &[
    ("bed Rain", 75075, [16.15316, 3.12591, 14.87466, 9.23113]),
    ("bed DrivingRain", 75075, [-11.56035, 6.66040, -12.80555, 7.48990]),
    ("bed Snowfall", 75075, [-0.23368, -0.16214, -0.42965, 0.19438]),
    ("bed Sandstorm", 75075, [-1.45337, 10.80610, -8.46829, -3.19566]),
    ("bed Blizzard", 75075, [-1.95516, 1.33949, -4.73817, -1.78786]),
    ("call Rattlesnake 0x0", 80080, [-6.97348, -2.07740, 6.56404, -3.78326]),
    ("call Rattlesnake 0x10203", 80080, [-10.04333, -0.44017, 1.13749, -2.26129]),
    ("call Rattlesnake 0xfedcba9876543210", 80080, [12.91951, 14.30284, 1.62295, 4.91613]),
    ("call Crow 0x0", 55055, [47.69889, 10.45412, -22.87443, 24.07085]),
    ("call Crow 0x10203", 55055, [6.96192, -25.30306, 5.82284, 46.13588]),
    ("call Crow 0xfedcba9876543210", 55055, [-21.81715, 15.99740, -25.17112, 26.64207]),
    ("call Eagle 0x0", 65065, [28.98983, 6.86640, 46.32582, 55.42906]),
    ("call Eagle 0x10203", 65065, [6.99825, -15.24564, 9.69421, -21.02498]),
    ("call Eagle 0xfedcba9876543210", 65065, [1.20862, -37.04444, 13.56195, -17.38928]),
    ("call Wolf 0x0", 380380, [-48.15280, 19.58573, -109.64794, -53.43318]),
    ("call Wolf 0x10203", 380380, [-120.48972, 20.45395, -132.55769, -14.62536]),
    ("call Wolf 0xfedcba9876543210", 380380, [156.38050, 173.75575, -157.00243, -78.42733]),
    ("call Cricket 0x0", 45045, [18.27520, -3.10288, -4.44538, -43.16450]),
    ("call Cricket 0x10203", 45045, [-3.81560, 6.62133, 4.83353, 20.00880]),
    ("call Cricket 0xfedcba9876543210", 45045, [-14.74555, 6.99188, 16.89307, 31.56857]),
    ("call Parrot 0x0", 85085, [-8.17830, -2.40361, -5.37675, -23.03340]),
    ("call Parrot 0x10203", 85085, [-4.62595, 31.01545, -22.88560, 4.83245]),
    ("call Parrot 0xfedcba9876543210", 85085, [-13.54914, 2.12278, 4.51390, 3.48718]),
];
