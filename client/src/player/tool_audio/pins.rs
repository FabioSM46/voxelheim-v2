//! Every tool sound as its player bakes it, pinned (see `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

const MATERIALS: [MaterialClass; 8] = [
    MaterialClass::Air,
    MaterialClass::Stone,
    MaterialClass::Earth,
    MaterialClass::Sand,
    MaterialClass::Wood,
    MaterialClass::Foliage,
    MaterialClass::Glass,
    MaterialClass::Water,
];
const TOOLS: [MiningTool; 4] = [
    MiningTool::Hand,
    MiningTool::Shovel,
    MiningTool::Pickaxe,
    MiningTool::Axe,
];

#[test]
fn every_tool_sound_bakes_identically_to_its_pin() {
    let mut cues = vec![];
    for tool in TOOLS {
        for material in MATERIALS {
            cues.push((
                format!("strike {tool:?} {material:?}"),
                Cue::Strike(tool, material),
            ));
            cues.push((
                format!("break {tool:?} {material:?}"),
                Cue::Break(tool, material),
            ));
        }
    }
    cues.push(("swing".into(), Cue::Swing(false)));
    cues.push(("swing weapon".into(), Cue::Swing(true)));
    let rendered: Vec<_> = cues
        .into_iter()
        .map(|(name, cue)| {
            let hash = pin::across_rates(|rate| {
                Tools::default()
                    .bake(cue, rate)
                    .expect("a tool cue bakes")
                    .samples()
                    .to_vec()
            });
            (name, hash)
        })
        .collect();
    pin::assert_pins(&rendered, PINS);
}

const PINS: &[(&str, u64)] = &[
    ("strike Hand Air", 0x3c90934765e273ed),
    ("break Hand Air", 0x0e045c7ca0b4ef21),
    ("strike Hand Stone", 0x2f77929e7496a2cd),
    ("break Hand Stone", 0x62424af26b659bc6),
    ("strike Hand Earth", 0x1a3ce1c55924faaf),
    ("break Hand Earth", 0xe0fb24279514bf19),
    ("strike Hand Sand", 0xe39d1a11bf438521),
    ("break Hand Sand", 0x9b1cdebb2ed3d8d5),
    ("strike Hand Wood", 0x79f77e53255e1ee5),
    ("break Hand Wood", 0xfffe33435709ff7e),
    ("strike Hand Foliage", 0xc3420ffd4e1dcd04),
    ("break Hand Foliage", 0x507a3c923a9a376f),
    ("strike Hand Glass", 0x2f77929e7496a2cd),
    ("break Hand Glass", 0x62424af26b659bc6),
    ("strike Hand Water", 0x3c90934765e273ed),
    ("break Hand Water", 0x0e045c7ca0b4ef21),
    ("strike Shovel Air", 0xe7c5dfabacc39375),
    ("break Shovel Air", 0x393fe4c1ccadd818),
    ("strike Shovel Stone", 0x3f4b613bb7d78e27),
    ("break Shovel Stone", 0xccff6f72ee0e9446),
    ("strike Shovel Earth", 0xd02fb15a0d233bdd),
    ("break Shovel Earth", 0x3576d3a545046ee6),
    ("strike Shovel Sand", 0x08b3751f65bd2ac6),
    ("break Shovel Sand", 0x14b6e24b11f446a0),
    ("strike Shovel Wood", 0x58491ba1599688da),
    ("break Shovel Wood", 0xec1875b0a8ff01e7),
    ("strike Shovel Foliage", 0x2327b962e5524bab),
    ("break Shovel Foliage", 0xf90fc8e24aa27a2c),
    ("strike Shovel Glass", 0x3f4b613bb7d78e27),
    ("break Shovel Glass", 0xccff6f72ee0e9446),
    ("strike Shovel Water", 0xe7c5dfabacc39375),
    ("break Shovel Water", 0x393fe4c1ccadd818),
    ("strike Pickaxe Air", 0x7b436662672ca93f),
    ("break Pickaxe Air", 0x966581a22b57df32),
    ("strike Pickaxe Stone", 0x832eab0a3295b22f),
    ("break Pickaxe Stone", 0xea4064fca9c69f44),
    ("strike Pickaxe Earth", 0x435495957998e697),
    ("break Pickaxe Earth", 0x6d792d982ed3b28a),
    ("strike Pickaxe Sand", 0xc1dc4eec785dca03),
    ("break Pickaxe Sand", 0xbec502e4238115c9),
    ("strike Pickaxe Wood", 0x51fd707cf6f66fc8),
    ("break Pickaxe Wood", 0xb843243b091600fd),
    ("strike Pickaxe Foliage", 0xeab421e4a31b548e),
    ("break Pickaxe Foliage", 0xd86c96cbd0e30ea0),
    ("strike Pickaxe Glass", 0x832eab0a3295b22f),
    ("break Pickaxe Glass", 0xea4064fca9c69f44),
    ("strike Pickaxe Water", 0x7b436662672ca93f),
    ("break Pickaxe Water", 0x966581a22b57df32),
    ("strike Axe Air", 0xe274c79205503c95),
    ("break Axe Air", 0x4c933b4b3ddcfdfd),
    ("strike Axe Stone", 0xfb82222ede7045e1),
    ("break Axe Stone", 0xaa1da89aec455bc6),
    ("strike Axe Earth", 0x45183a8c45c5aa5a),
    ("break Axe Earth", 0xf50dd659fc6d2c58),
    ("strike Axe Sand", 0x8aef9f95fca4bb81),
    ("break Axe Sand", 0x893a7bcd6e2c2672),
    ("strike Axe Wood", 0xc185e9ae745ffc46),
    ("break Axe Wood", 0xe7fb0ca3f4e3cb62),
    ("strike Axe Foliage", 0xae1ccbfe612ad92f),
    ("break Axe Foliage", 0x240c858acbbaa5e6),
    ("strike Axe Glass", 0xfb82222ede7045e1),
    ("break Axe Glass", 0xaa1da89aec455bc6),
    ("strike Axe Water", 0xe274c79205503c95),
    ("break Axe Water", 0x4c933b4b3ddcfdfd),
    ("swing", 0xc780c570165933b1),
    ("swing weapon", 0x96556bfff2306d25),
];
