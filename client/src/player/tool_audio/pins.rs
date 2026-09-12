//! Every tool sound as its player bakes it, pinned bit for bit (see `audio::synth::pin`).
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
fn every_tool_sound_bakes_bit_identical_to_its_pin() {
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
    ("strike Hand Air", 0xd7986b52c86e8807),
    ("break Hand Air", 0x43b233e8d9c898f3),
    ("strike Hand Stone", 0xaabc31176b168905),
    ("break Hand Stone", 0x2afd46a7f0927731),
    ("strike Hand Earth", 0xc19f3f15602aa00a),
    ("break Hand Earth", 0x9659ce8d3125dbd2),
    ("strike Hand Sand", 0x432885e0ce9e0c53),
    ("break Hand Sand", 0xeb8f6a55364d60ca),
    ("strike Hand Wood", 0xbe87272926b8a0c0),
    ("break Hand Wood", 0xe497c633d3ca3e7b),
    ("strike Hand Foliage", 0xf33dec38854a9f2f),
    ("break Hand Foliage", 0xf76a5c192f025d1b),
    ("strike Hand Glass", 0xaabc31176b168905),
    ("break Hand Glass", 0x2afd46a7f0927731),
    ("strike Hand Water", 0xd7986b52c86e8807),
    ("break Hand Water", 0x43b233e8d9c898f3),
    ("strike Shovel Air", 0xa220b4785df94afb),
    ("break Shovel Air", 0xd0045799ac3bd1d6),
    ("strike Shovel Stone", 0x4281a03e5c9b784c),
    ("break Shovel Stone", 0x624b427b4abfc5b6),
    ("strike Shovel Earth", 0x82ea70ccb3e60f3b),
    ("break Shovel Earth", 0x7ac621d1683215e6),
    ("strike Shovel Sand", 0x2917e2f344cd8ba7),
    ("break Shovel Sand", 0x4d0282fd0f8982ad),
    ("strike Shovel Wood", 0x4dc54ff7726bb68a),
    ("break Shovel Wood", 0xa3fa5e634c14c088),
    ("strike Shovel Foliage", 0x9be56b2f8dd9fbee),
    ("break Shovel Foliage", 0x92d4d5b6a6c3334c),
    ("strike Shovel Glass", 0x4281a03e5c9b784c),
    ("break Shovel Glass", 0x624b427b4abfc5b6),
    ("strike Shovel Water", 0xa220b4785df94afb),
    ("break Shovel Water", 0xd0045799ac3bd1d6),
    ("strike Pickaxe Air", 0x27533137e5fd8c2f),
    ("break Pickaxe Air", 0xbf674208384f92aa),
    ("strike Pickaxe Stone", 0x443bc1fd87df4e05),
    ("break Pickaxe Stone", 0x6532836db6a8cf6f),
    ("strike Pickaxe Earth", 0x384b30fc4763eb4e),
    ("break Pickaxe Earth", 0xdd7e2c2b61e00974),
    ("strike Pickaxe Sand", 0x9bed0765a8299a01),
    ("break Pickaxe Sand", 0x330ff7dd8b764e8c),
    ("strike Pickaxe Wood", 0x97ddf8989966cf00),
    ("break Pickaxe Wood", 0x18a301ba6b9a5ea6),
    ("strike Pickaxe Foliage", 0x1a2c7740f78dde36),
    ("break Pickaxe Foliage", 0xc66d69ec73cbc618),
    ("strike Pickaxe Glass", 0x443bc1fd87df4e05),
    ("break Pickaxe Glass", 0x6532836db6a8cf6f),
    ("strike Pickaxe Water", 0x27533137e5fd8c2f),
    ("break Pickaxe Water", 0xbf674208384f92aa),
    ("strike Axe Air", 0x4ebf54c8f4502d55),
    ("break Axe Air", 0xf9683596d4cb26ee),
    ("strike Axe Stone", 0x153363366f5e572c),
    ("break Axe Stone", 0xdf08f26b6f952406),
    ("strike Axe Earth", 0x905be78962da8fcc),
    ("break Axe Earth", 0x03431b4f4931d688),
    ("strike Axe Sand", 0x1301c95e0cb90f35),
    ("break Axe Sand", 0xaed33e0e961044ef),
    ("strike Axe Wood", 0x57b33fea05d800f9),
    ("break Axe Wood", 0xe2d18e1d93167257),
    ("strike Axe Foliage", 0xa903c1d627948dbe),
    ("break Axe Foliage", 0xa65357d4eb2ea8db),
    ("strike Axe Glass", 0x153363366f5e572c),
    ("break Axe Glass", 0xdf08f26b6f952406),
    ("strike Axe Water", 0x4ebf54c8f4502d55),
    ("break Axe Water", 0xf9683596d4cb26ee),
    ("swing", 0xaed96d68f63d9f7a),
    ("swing weapon", 0x7c146a07c65f7e69),
];
