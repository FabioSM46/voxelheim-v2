//! The arrival chime as it is baked, pinned bit for bit (see `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

#[test]
fn the_arrival_chime_bakes_bit_identical_to_its_pin() {
    let rendered = [("chime".to_string(), pin::baked(&description(), 0.32, 0))];
    pin::assert_pins(&rendered, PINS);
}

const PINS: &[(&str, u64)] = &[("chime", 0x4d323192d3bd303a)];
