//! The arrival chime as it is baked, pinned (see `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

#[test]
fn the_arrival_chime_bakes_identically_to_its_pin() {
    let rendered = [("chime".to_string(), pin::baked(&description(), 0.32, 0))];
    pin::assert_pins(&rendered, PINS);
}

#[rustfmt::skip]
const PINS: &[pin::Row] = &[
    ("chime", 32032, [21.37576, 5.70463, 17.60261, -1.02038]),
];
