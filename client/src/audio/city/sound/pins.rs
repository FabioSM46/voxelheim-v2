//! Every bench and fire sound in the palette, pinned bit for bit (see `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

#[test]
fn every_city_sound_renders_bit_identical_to_its_pin() {
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
    ("hammer", 0x5cc83b336315f8e0),
    ("pop", 0x21a704e156e3a9c6),
    ("scrape", 0x7b6798fe8f06a090),
    ("tap", 0xed34527fc3e189e0),
    ("hum", 0xd8e45975a12c710d),
    ("fire bed", 0xbbe2e178fe95ef8e),
];
