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

const PINS: &[(&str, u64)] = &[
    ("hammer", 0x6e6f3c74608617e1),
    ("pop", 0x341557d8ee59176d),
    ("scrape", 0x7519af547789960c),
    ("tap", 0xbc7085b0480052de),
    ("hum", 0x796f072ecce05b2e),
    ("fire bed", 0x863da026596a6e6b),
];
