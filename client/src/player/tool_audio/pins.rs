//! Every tool sound as its player bakes it, pinned (see `audio::synth::pin`).
use super::*;
use crate::audio::synth::pin;

const MATERIALS: [MaterialClass; 9] = [
    MaterialClass::Air,
    MaterialClass::Stone,
    MaterialClass::Earth,
    MaterialClass::Snow,
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

#[rustfmt::skip]
const PINS: &[pin::Row] = &[
    ("strike Hand Air", 24024, [7.84559, -5.36651, 7.77951, -6.01640]),
    ("break Hand Air", 38038, [-0.26441, 14.46593, -24.74246, 2.54130]),
    ("strike Hand Stone", 24024, [18.33328, 4.59297, 19.58701, -11.51855]),
    ("break Hand Stone", 38038, [5.65929, 19.73441, -32.11750, -2.54141]),
    ("strike Hand Earth", 24024, [8.02094, -1.24954, 8.55930, -1.93779]),
    ("break Hand Earth", 38038, [-2.15713, 14.89000, -27.36528, 6.09777]),
    ("strike Hand Snow", 24024, [9.89329, 7.53230, -0.34692, -3.73432]),
    ("break Hand Snow", 38038, [18.39377, 7.18969, -28.66405, -6.76323]),
    ("strike Hand Sand", 24024, [-8.22666, -38.46121, 44.19273, 31.28140]),
    ("break Hand Sand", 38038, [-6.03116, 23.23372, -19.80290, 8.89601]),
    ("strike Hand Wood", 24024, [25.47542, 10.25864, 12.72800, -17.96882]),
    ("break Hand Wood", 38038, [37.61874, 13.72871, -5.63773, -3.90646]),
    ("strike Hand Foliage", 24024, [-9.01167, 15.01750, 40.09224, 6.99442]),
    ("break Hand Foliage", 38038, [-9.62681, 18.37029, -56.46319, -3.71304]),
    ("strike Hand Glass", 24024, [18.33328, 4.59297, 19.58701, -11.51855]),
    ("break Hand Glass", 38038, [5.65929, 19.73441, -32.11750, -2.54141]),
    ("strike Hand Water", 24024, [7.84559, -5.36651, 7.77951, -6.01640]),
    ("break Hand Water", 38038, [-0.26441, 14.46593, -24.74246, 2.54130]),
    ("strike Shovel Air", 24024, [-12.69091, -15.37095, -1.21200, -1.24924]),
    ("break Shovel Air", 38038, [-2.16122, 22.99549, -18.25592, 18.87886]),
    ("strike Shovel Stone", 24024, [-3.62485, -15.11446, 14.47366, -4.05625]),
    ("break Shovel Stone", 38038, [3.76248, 28.26397, -25.63096, 13.79615]),
    ("strike Shovel Earth", 24024, [-12.51556, -11.25398, -0.43221, 2.82936]),
    ("break Shovel Earth", 38038, [-4.05394, 23.41956, -20.87874, 22.43532]),
    ("strike Shovel Snow", 24024, [-12.06483, -12.17513, -5.46027, 3.72799]),
    ("break Shovel Snow", 38038, [16.49696, 15.71925, -22.17751, 9.57433]),
    ("strike Shovel Sand", 24024, [-28.76317, -48.46565, 35.20122, 36.04855]),
    ("break Shovel Sand", 38038, [-8.12697, 32.11596, -13.51537, 24.88088]),
    ("strike Shovel Wood", 24024, [3.51729, -9.44879, 7.61465, -10.50651]),
    ("break Shovel Wood", 38038, [35.72193, 22.25827, 0.84881, 12.43110]),
    ("strike Shovel Foliage", 24024, [-29.47019, 5.21282, 31.17293, 11.55602]),
    ("break Shovel Foliage", 38038, [-11.27246, 26.78988, -49.94500, 12.36944]),
    ("strike Shovel Glass", 24024, [-3.62485, -15.11446, 14.47366, -4.05625]),
    ("break Shovel Glass", 38038, [3.76248, 28.26397, -25.63096, 13.79615]),
    ("strike Shovel Water", 24024, [-12.69091, -15.37095, -1.21200, -1.24924]),
    ("break Shovel Water", 38038, [-2.16122, 22.99549, -18.25592, 18.87886]),
    ("strike Pickaxe Air", 24024, [-26.62210, -0.48233, -6.27327, 6.55499]),
    ("break Pickaxe Air", 38038, [7.08075, 11.58579, -0.13442, 15.51692]),
    ("strike Pickaxe Stone", 24024, [-14.77545, 4.72938, 6.55440, -2.98343]),
    ("break Pickaxe Stone", 38038, [1.77026, 23.63630, -21.44081, 2.32935]),
    ("strike Pickaxe Earth", 24024, [-23.09361, 2.24824, -4.75920, 4.82589]),
    ("break Pickaxe Earth", 38038, [11.58767, 11.21152, -4.48297, 13.28014]),
    ("strike Pickaxe Snow", 24024, [-26.07166, 2.06601, -5.17128, 2.34734]),
    ("break Pickaxe Snow", 38038, [15.56632, 10.53687, -16.72170, 0.04701]),
    ("strike Pickaxe Sand", 24024, [-9.64431, 10.52603, -36.15815, 44.37425]),
    ("break Pickaxe Sand", 38038, [26.28374, 5.99797, -23.49339, 43.49328]),
    ("strike Pickaxe Wood", 24024, [-7.63331, 10.39505, -0.30461, -9.43369]),
    ("break Pickaxe Wood", 38038, [33.81062, 17.71150, 5.11986, 1.04520]),
    ("strike Pickaxe Foliage", 24024, [-26.97879, -2.91306, -28.84906, 15.14185]),
    ("break Pickaxe Foliage", 38038, [-14.01582, 3.25753, 40.80749, 44.98838]),
    ("strike Pickaxe Glass", 24024, [-14.77545, 4.72938, 6.55440, -2.98343]),
    ("break Pickaxe Glass", 38038, [1.77026, 23.63630, -21.44081, 2.32935]),
    ("strike Pickaxe Water", 24024, [-26.62210, -0.48233, -6.27327, 6.55499]),
    ("break Pickaxe Water", 38038, [7.08075, 11.58579, -0.13442, 15.51692]),
    ("strike Axe Air", 24024, [-6.35449, -1.31049, -0.06270, -3.05773]),
    ("break Axe Air", 38038, [20.81221, 23.66551, -4.67238, 16.20232]),
    ("strike Axe Stone", 24024, [2.71157, -1.05401, 15.62297, -5.86473]),
    ("break Axe Stone", 38038, [26.73591, 28.93399, -12.04741, 11.11962]),
    ("strike Axe Earth", 24024, [-6.17915, 2.80648, 0.71709, 1.02088]),
    ("break Axe Earth", 38038, [18.91949, 24.08958, -7.29519, 19.75879]),
    ("strike Axe Snow", 24024, [-5.72842, 1.88533, -4.31096, 1.91951]),
    ("break Axe Snow", 38038, [39.47039, 16.38927, -8.59397, 6.89780]),
    ("strike Axe Sand", 24024, [-22.42675, -34.40519, 36.35052, 34.24007]),
    ("break Axe Sand", 38038, [15.04546, 32.43329, 0.26718, 22.55703]),
    ("strike Axe Wood", 24024, [9.85371, 4.61166, 8.76395, -12.31500]),
    ("break Axe Wood", 38038, [58.69536, 22.92829, 14.43235, 9.75456]),
    ("strike Axe Foliage", 24024, [-23.21176, 19.07351, 32.25003, 9.95309]),
    ("break Axe Foliage", 38038, [11.71370, 27.42329, -36.33299, 9.86584]),
    ("strike Axe Glass", 24024, [2.71157, -1.05401, 15.62297, -5.86473]),
    ("break Axe Glass", 38038, [26.73591, 28.93399, -12.04741, 11.11962]),
    ("strike Axe Water", 24024, [-6.35449, -1.31049, -0.06270, -3.05773]),
    ("break Axe Water", 38038, [20.81221, 23.66551, -4.67238, 16.20232]),
    ("swing", 20020, [3.06933, -3.02903, 3.48543, -9.99192]),
    ("swing weapon", 20020, [-2.88251, 7.34889, -11.50221, 20.24442]),
];
