//! Every bench and fire sound in the palette, pinned (see `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

#[test]
fn every_city_sound_renders_identically_to_its_pin() {
    let transient = |pick: fn(&Palette) -> &Baked| {
        pin::across_rates(|rate| {
            pick(&Palette::new(rate).expect("the palette bakes"))
                .samples()
                .to_vec()
        })
    };
    let rendered = vec![
        ("hammer".to_string(), transient(|palette| &palette.hammer)),
        ("pop".to_string(), transient(|palette| &palette.pop)),
        ("scrape".to_string(), transient(|palette| &palette.scrape)),
        ("tap".to_string(), transient(|palette| &palette.tap)),
        ("hum".to_string(), transient(|palette| &palette.hum)),
        (
            "fire bed".to_string(),
            pin::continuous(&fire(), 1.0, 0x5eed),
        ),
    ];
    pin::assert_pins(&rendered, PINS);
}

#[rustfmt::skip]
const PINS: &[pin::Row] = &[
    ("hammer", 28028, [-8.59460, -8.72085, 26.48228, 6.20611]),
    ("pop", 2503, [-0.05365, 2.88945, 3.29109, 5.94482]),
    ("scrape", 26026, [0.36233, -13.06267, 15.37310, -26.58897]),
    ("tap", 14014, [-3.20487, -5.82116, -3.42102, 5.40944]),
    ("hum", 240240, [37.54800, -8.33806, 29.50909, 51.58334]),
    ("fire bed", 100100, [-0.25196, -0.28026, -4.86707, 0.51337]),
];
