//! Block id to colour, and the one question the mesher asks about a block.
//!
//! No texture atlas, no UVs, no art asset — the ids the server sends are the whole
//! material system for now, and a colour per id is enough to read the landscape.
//! Deliberately not a Bevy `Color`: everything here is plain arithmetic so the
//! mesher stays testable without an app.
//!
//! The ids are wire values. They come from `server/internal/world/chunk.go`, where
//! the palette is documented as *append, never renumber* — so an id this build has
//! no colour for is a newer server, not a corrupt one, and it gets a colour that
//! shouts rather than an error.

use super::BlockId;

/// Empty space. Never meshed, and the only id that is not solid.
pub const AIR: BlockId = 0;
/// The bulk of the ground.
pub const STONE: BlockId = 1;
/// The thin layer under the surface.
pub const DIRT: BlockId = 2;
/// The surface below the snow line.
pub const GRASS: BlockId = 3;
/// The surface at or above the snow line.
pub const SNOW: BlockId = 4;
/// Conifer trunks.
pub const LOG: BlockId = 5;
/// Opaque conifer foliage.
pub const LEAVES: BlockId = 6;
/// Coal-bearing stone.
pub const COAL_ORE: BlockId = 7;
/// Iron-bearing stone.
pub const IRON_ORE: BlockId = 8;
/// The desert surface.
pub const SAND: BlockId = 9;
/// What sits under a desert's sand.
pub const SANDSTONE: BlockId = 10;
/// The loose patches that break up plains and taiga soil.
pub const GRAVEL: BlockId = 11;
/// Still water. A body moves through every member of the water family; none is
/// solid or opaque.
pub const WATER: BlockId = 12;
/// The lid on some of the water. Ordinary ground: solid, opaque, walked on.
pub const ICE: BlockId = 13;
/// Sawn timber: what a settlement's walls and a keep's roof are made of.
pub const PLANKS: BlockId = 14;
/// Dressed rubble: footings, a smithy's walls and the whole of a keep.
pub const COBBLESTONE: BlockId = 15;
/// Straw: what every roof but the keep's is thatched with.
pub const THATCH: BlockId = 16;
/// Palm trunks. Mirrors the server's `world.PalmLog`.
pub const PALM_LOG: BlockId = 17;
/// Palm crowns. Mirrors the server's `world.PalmFronds`.
pub const PALM_FRONDS: BlockId = 18;
/// Dry desert scrub. Mirrors the server's `world.DesertShrub`.
pub const DESERT_SHRUB: BlockId = 19;
/// Broadleaf crowns. Mirrors the server's `world.BroadLeaves`.
pub const BROAD_LEAVES: BlockId = 20;
/// Low plains cover. Mirrors the server's `world.Bush`.
pub const BUSH: BlockId = 21;
// Flowing levels encode their height in eighths; currents are full height.
pub const WATER_FLOW1: BlockId = 22;
pub const WATER_FLOW2: BlockId = 23;
pub const WATER_FLOW3: BlockId = 24;
pub const WATER_FLOW4: BlockId = 25;
pub const WATER_FLOW5: BlockId = 26;
pub const WATER_FLOW6: BlockId = 27;
pub const WATER_FLOW7: BlockId = 28;
pub const WATER_CURRENT_XPOS: BlockId = 29;
pub const WATER_CURRENT_XNEG: BlockId = 30;
pub const WATER_CURRENT_ZPOS: BlockId = 31;
pub const WATER_CURRENT_ZNEG: BlockId = 32;

// Cover: the ids a body walks straight through and a ray still stops on. Mirrors the
// server's `world.Cover` (#550), which is a third answer beside solid and fluid rather
// than the complement of either.
/// Red meadow flowers. Mirrors the server's `world.FlowerRed`.
pub const FLOWER_RED: BlockId = 33;
/// Yellow meadow flowers. Mirrors the server's `world.FlowerYellow`.
pub const FLOWER_YELLOW: BlockId = 34;
/// Blue meadow flowers. Mirrors the server's `world.FlowerBlue`.
pub const FLOWER_BLUE: BlockId = 35;

const COVER_FAMILY: [BlockId; 3] = [FLOWER_RED, FLOWER_YELLOW, FLOWER_BLUE];

const WATER_FAMILY: [BlockId; 12] = [
    WATER,
    WATER_FLOW1,
    WATER_FLOW2,
    WATER_FLOW3,
    WATER_FLOW4,
    WATER_FLOW5,
    WATER_FLOW6,
    WATER_FLOW7,
    WATER_CURRENT_XPOS,
    WATER_CURRENT_XNEG,
    WATER_CURRENT_ZPOS,
    WATER_CURRENT_ZNEG,
];

/// Whether `block` belongs to the whole water family.
pub fn is_water(block: BlockId) -> bool {
    WATER_FAMILY.contains(&block)
}

/// Whether `block` is cover — a thing standing in a voxel rather than filling it.
///
/// The client's mirror of the server's `world.Cover`, and the third answer this
/// palette gives about a block: cover stops no body ([`is_solid`] is false) and hides
/// nothing ([`is_opaque`] is false), yet it is still a voxel a player can outline and
/// break. `ChunkStore::targetable_at` is what puts those two facts together; nothing
/// else needs to ask.
///
/// Not the complement of anything. Air is neither solid nor cover, water is neither,
/// and an id from a newer contract is solid rather than cover for the reason it is
/// opaque — this build draws what the server sent.
pub fn is_cover(block: BlockId) -> bool {
    COVER_FAMILY.contains(&block)
}

/// The server-authored water height in eighths. Falling is resolved by the mesher.
pub fn water_level(block: BlockId) -> u8 {
    match block {
        WATER | WATER_CURRENT_XPOS..=WATER_CURRENT_ZNEG => 8,
        WATER_FLOW1..=WATER_FLOW7 => (block - WATER_FLOW1 + 1) as u8,
        _ => 0,
    }
}

/// The horizontal current encoded by a water id, as `(x, z)`.
///
/// The mesher's `flow_at` is the one caller: a current id is already
/// a direction, so it becomes a flow vector verbatim rather than through a gradient.
pub fn current_of(block: BlockId) -> (i8, i8) {
    match block {
        WATER_CURRENT_XPOS => (1, 0),
        WATER_CURRENT_XNEG => (-1, 0),
        WATER_CURRENT_ZPOS => (0, 1),
        WATER_CURRENT_ZNEG => (0, -1),
        _ => (0, 0),
    }
}

/// Whether a block stops a body and can be aimed at.
///
/// The predicate everything outside the mesher asks — the aiming ray, the camera boom,
/// the store's `solid_at` — and the client's mirror of the server's `world.Solid`.
/// Water is *there* without stopping anything, so a ray passes through the whole
/// family and the block behind it is what gets outlined. Cover is there without
/// stopping anything either, and the aiming ray is the one caller that wants it
/// anyway — which is why that ray asks `ChunkStore::targetable_at` instead of this.
pub fn is_solid(block: BlockId) -> bool {
    block != AIR && !is_water(block) && !is_cover(block)
}

/// Whether a block hides what is behind it.
///
/// The predicate the **mesher** asks, and a second function rather than a second caller
/// of [`is_solid`] because the two answer different questions that happen to agree on
/// every id today: a face is culled when the voxel across it hides it, a ray stops when
/// the voxel in front of it blocks the body. Glass is what will separate them, and when
/// it arrives only this function has to learn about it.
///
/// An id from a newer contract is opaque, for the reason it is solid: this build draws
/// what the server sent rather than deciding an id it never heard of is see-through.
///
/// Cover is not opaque, and that is the whole of what the two masks need to learn about
/// a flower: the grass under one keeps its top face, and the water beside one keeps its
/// surface, because both mask arms already read "see-through" rather than "air".
pub fn is_opaque(block: BlockId) -> bool {
    block != AIR && !is_water(block) && !is_cover(block)
}

/// The palette in the order a reader wants to see it. Test-only: production code
/// asks [`linear_rgba`] about one block at a time.
#[cfg(test)]
pub const PALETTE: [BlockId; 35] = [
    STONE,
    DIRT,
    GRASS,
    SNOW,
    LOG,
    LEAVES,
    COAL_ORE,
    IRON_ORE,
    SAND,
    SANDSTONE,
    GRAVEL,
    WATER,
    ICE,
    PLANKS,
    COBBLESTONE,
    THATCH,
    PALM_LOG,
    PALM_FRONDS,
    DESERT_SHRUB,
    BROAD_LEAVES,
    BUSH,
    WATER_FLOW1,
    WATER_FLOW2,
    WATER_FLOW3,
    WATER_FLOW4,
    WATER_FLOW5,
    WATER_FLOW6,
    WATER_FLOW7,
    WATER_CURRENT_XPOS,
    WATER_CURRENT_XNEG,
    WATER_CURRENT_ZPOS,
    WATER_CURRENT_ZNEG,
    FLOWER_RED,
    FLOWER_YELLOW,
    FLOWER_BLUE,
];

/// How much of what is behind it a voxel of water lets through — 0 is invisible, 1 is a
/// solid wall of blue. Low enough that a lake bed a few blocks down is legible from the
/// shore, high enough that the surface is a surface rather than a tint.
pub const WATER_ALPHA: f32 = 0.62;

// The colours, **linear**, which is the space vertex colours are multiplied into the
// material's `base_color` in. Each is the sRGB value in its doc comment run through
// the sRGB transfer function.
//
// Written out rather than converted at runtime, because the transfer function needs
// `powf` and is therefore not const-evaluable, and the mesher would otherwise pay
// three of them per merged quad. `linear_values_match_their_srgb` in the tests below
// is what keeps the pair honest: change a colour, run the tests, and paste the
// numbers the failure prints.

/// Slate grey, faintly cool — Fimbulvetr rock rather than warm sandstone. `#78787D`.
const STONE_LINEAR: [f32; 3] = [0.187_821, 0.187_821, 0.205_079];

/// Wet earth. `#6B4F32`.
const DIRT_LINEAR: [f32; 3] = [0.147_027, 0.078_187, 0.031_896];

/// A dark northern green; nothing here is a spring meadow. `#4F7A3A`.
const GRASS_LINEAR: [f32; 3] = [0.078_187, 0.194_618, 0.042_311];

/// Just off white, so snow still reads as a surface under flat light. `#F2F5F7`.
const SNOW_LINEAR: [f32; 3] = [0.887_923, 0.913_099, 0.930_111];

/// Dark conifer bark. `#593D28`.
const LOG_LINEAR: [f32; 3] = [0.099_899, 0.046_665, 0.021_219];

/// Dense winter needles. `#294F38`.
const LEAVES_LINEAR: [f32; 3] = [0.022_174, 0.078_187, 0.039_546];

/// Coal flecks against dark stone. `#30343A`.
const COAL_ORE_LINEAR: [f32; 3] = [0.029_557, 0.034_340, 0.042_311];

/// Cold rock stained with oxidised iron. `#9A6543`.
const IRON_ORE_LINEAR: [f32; 3] = [0.323_143, 0.130_136, 0.056_128];

/// Pale, faintly warm sand — a cold-world desert rather than a beach. `#C9B383`.
const SAND_LINEAR: [f32; 3] = [0.584_078, 0.450_786, 0.226_966];

/// Compacted sand, darker and more orange than the loose grains above it. `#A8865A`.
const SANDSTONE_LINEAR: [f32; 3] = [0.391_572, 0.238_398, 0.102_242];

/// Wet grey shingle, cooler and darker than stone so a patch reads against it. `#5C5F63`.
const GRAVEL_LINEAR: [f32; 3] = [0.107_023, 0.114_435, 0.124_772];

/// Deep northern lake water. `#1A4D8C`.
///
/// Carries [`WATER_ALPHA`], and it is the **only** place water's translucency is written
/// down: `world/render.rs` gives the water mesh a white material for the reason the
/// terrain material is white, so this swatch reaches the framebuffer once rather than
/// being multiplied into itself.
const WATER_LINEAR: [f32; 3] = [0.010_330, 0.074_214, 0.262_251];

/// Pale blue-grey lake ice — cold, and darker than snow so a frozen surface reads
/// against the bank it meets. `#A6CBD8`.
const ICE_LINEAR: [f32; 3] = [0.381_326, 0.597_202, 0.686_685];

/// Sawn pine, warmer and lighter than the bark it came off, so a wall reads against the
/// forest behind it. `#B08640`.
const PLANKS_LINEAR: [f32; 3] = [0.434_154, 0.238_398, 0.051_269];

/// Dressed rubble: the same slate as the ground it was quarried from, a shade lighter
/// and warmer, so a wall is legible against the hillside behind it. `#8A8A86`.
const COBBLESTONE_LINEAR: [f32; 3] = [0.254_152, 0.254_152, 0.238_398];

/// Dry straw, the brightest thing in a settlement — a roof is what is seen first from a
/// distance. `#C7A24E`.
const THATCH_LINEAR: [f32; 3] = [0.571_125, 0.361_307, 0.076_185];

/// Palm bark, paler and greyer than the conifer's dark brown. `#806B52`.
const PALM_LOG_LINEAR: [f32; 3] = [0.215_861, 0.147_027, 0.084_376];

/// Sunlit yellow-green palm fronds, distinctly lighter than conifer needles. `#829C3D`.
const PALM_FRONDS_LINEAR: [f32; 3] = [0.223_228, 0.332_452, 0.046_665];

/// Dry olive-khaki scrub against pale desert sand. `#8D8250`.
const DESERT_SHRUB_LINEAR: [f32; 3] = [0.266_356, 0.223_228, 0.080_220];

/// A brighter, warmer crown than the conifer's winter green. `#4F8D43`.
const BROAD_LEAVES_LINEAR: [f32; 3] = [0.078_187, 0.266_356, 0.056_128];

/// Low green cover between dark conifer needles and a broadleaf crown. `#396B3D`.
const BUSH_LINEAR: [f32; 3] = [0.040_915, 0.147_027, 0.046_665];

/// A deep meadow red, dark enough to read against a sunlit grass field and saturated
/// enough to hold its hue under the night ambient floor `player/sky.rs` sets. `#C4383A`.
const FLOWER_RED_LINEAR: [f32; 3] = [0.552_011, 0.039_546, 0.042_311];

/// The brightest of the three, and the one seen furthest into the fog. `#E8C64A`.
const FLOWER_YELLOW_LINEAR: [f32; 3] = [0.806_952, 0.564_712, 0.068_478];

/// A cool blue that stays separable from lake water at distance — lighter and far less
/// saturated than [`WATER_LINEAR`], which is what keeps a shore drift from reading as a
/// puddle. `#5B76C8`.
const FLOWER_BLUE_LINEAR: [f32; 3] = [0.104_616, 0.181_164, 0.577_580];

/// The colour of "this build has no colour for that id". `#C81E96`.
///
/// Magenta on purpose: a server one contract ahead sends a block this client has
/// never heard of, and the honest answer is to draw it loudly rather than guess a
/// plausible grey that hides the version skew.
const UNKNOWN_LINEAR: [f32; 3] = [0.577_580, 0.012_983, 0.304_987];

/// The linear RGBA a vertex of this block's faces carries.
///
/// Alpha is 1 for every id outside the water family, whose members carry
/// [`WATER_ALPHA`]. The renderer draws water in a pass of its own with a blending
/// material, and this is what that pass fades by.
pub fn linear_rgba(block: BlockId) -> [f32; 4] {
    if is_water(block) {
        return [
            WATER_LINEAR[0],
            WATER_LINEAR[1],
            WATER_LINEAR[2],
            WATER_ALPHA,
        ];
    }
    let [r, g, b] = match block {
        STONE => STONE_LINEAR,
        DIRT => DIRT_LINEAR,
        GRASS => GRASS_LINEAR,
        SNOW => SNOW_LINEAR,
        LOG => LOG_LINEAR,
        LEAVES => LEAVES_LINEAR,
        COAL_ORE => COAL_ORE_LINEAR,
        IRON_ORE => IRON_ORE_LINEAR,
        SAND => SAND_LINEAR,
        SANDSTONE => SANDSTONE_LINEAR,
        GRAVEL => GRAVEL_LINEAR,
        ICE => ICE_LINEAR,
        PLANKS => PLANKS_LINEAR,
        COBBLESTONE => COBBLESTONE_LINEAR,
        THATCH => THATCH_LINEAR,
        PALM_LOG => PALM_LOG_LINEAR,
        PALM_FRONDS => PALM_FRONDS_LINEAR,
        DESERT_SHRUB => DESERT_SHRUB_LINEAR,
        BROAD_LEAVES => BROAD_LEAVES_LINEAR,
        BUSH => BUSH_LINEAR,
        // A cover block's colour is its **head**. A stem is a green of its own, and
        // whatever draws one asks for that by name, because a flower is two colours in
        // one voxel and this function answers per id.
        FLOWER_RED => FLOWER_RED_LINEAR,
        FLOWER_YELLOW => FLOWER_YELLOW_LINEAR,
        FLOWER_BLUE => FLOWER_BLUE_LINEAR,
        // `AIR` lands here with everything else, and correctly so: asking for the
        // colour of nothing is a meshing bug, and magenta is how it announces itself
        // instead of hiding as a plausible shade.
        _ => UNKNOWN_LINEAR,
    };
    [r, g, b, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sRGB electro-optical transfer function, from the sRGB standard. The
    /// reference the constants above are checked against.
    fn srgb_to_linear(byte: u8) -> f32 {
        let value = f32::from(byte) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    /// The colours as they are written in the doc comments above — the readable
    /// definition each linear constant is derived from.
    const SRGB: [(&str, [u8; 3], [f32; 3]); 25] = [
        ("stone", [0x78, 0x78, 0x7D], STONE_LINEAR),
        ("dirt", [0x6B, 0x4F, 0x32], DIRT_LINEAR),
        ("grass", [0x4F, 0x7A, 0x3A], GRASS_LINEAR),
        ("snow", [0xF2, 0xF5, 0xF7], SNOW_LINEAR),
        ("log", [0x59, 0x3D, 0x28], LOG_LINEAR),
        ("leaves", [0x29, 0x4F, 0x38], LEAVES_LINEAR),
        ("coal ore", [0x30, 0x34, 0x3A], COAL_ORE_LINEAR),
        ("iron ore", [0x9A, 0x65, 0x43], IRON_ORE_LINEAR),
        ("sand", [0xC9, 0xB3, 0x83], SAND_LINEAR),
        ("sandstone", [0xA8, 0x86, 0x5A], SANDSTONE_LINEAR),
        ("gravel", [0x5C, 0x5F, 0x63], GRAVEL_LINEAR),
        ("water", [0x1A, 0x4D, 0x8C], WATER_LINEAR),
        ("ice", [0xA6, 0xCB, 0xD8], ICE_LINEAR),
        ("planks", [0xB0, 0x86, 0x40], PLANKS_LINEAR),
        ("cobblestone", [0x8A, 0x8A, 0x86], COBBLESTONE_LINEAR),
        ("thatch", [0xC7, 0xA2, 0x4E], THATCH_LINEAR),
        ("palm log", [0x80, 0x6B, 0x52], PALM_LOG_LINEAR),
        ("palm fronds", [0x82, 0x9C, 0x3D], PALM_FRONDS_LINEAR),
        ("desert shrub", [0x8D, 0x82, 0x50], DESERT_SHRUB_LINEAR),
        ("broad leaves", [0x4F, 0x8D, 0x43], BROAD_LEAVES_LINEAR),
        ("bush", [0x39, 0x6B, 0x3D], BUSH_LINEAR),
        ("flower red", [0xC4, 0x38, 0x3A], FLOWER_RED_LINEAR),
        ("flower yellow", [0xE8, 0xC6, 0x4A], FLOWER_YELLOW_LINEAR),
        ("flower blue", [0x5B, 0x76, 0xC8], FLOWER_BLUE_LINEAR),
        ("unknown", [0xC8, 0x1E, 0x96], UNKNOWN_LINEAR),
    ];

    #[test]
    fn linear_values_match_their_srgb() {
        // What stops a hand-written linear constant from drifting away from the colour
        // its comment claims. If this fails, the message prints the numbers to paste.
        for (name, srgb, linear) in SRGB {
            let expected: Vec<f32> = srgb.iter().map(|byte| srgb_to_linear(*byte)).collect();
            for (channel, (got, want)) in linear.iter().zip(&expected).enumerate() {
                assert!(
                    (got - want).abs() < 1e-6,
                    "{name} channel {channel}: the constant says {got}, sRGB {srgb:?} says \
                     {want} (whole colour: {expected:?})",
                );
            }
        }
    }

    #[test]
    fn air_water_and_cover_are_the_ids_a_body_moves_through() {
        assert!(!is_solid(AIR));
        assert!(!is_solid(WATER), "water is swum through, not walked into");
        for block in PALETTE {
            if is_water(block) || is_cover(block) {
                assert!(!is_solid(block), "block {block} must stop no body");
                continue;
            }
            assert!(is_solid(block), "block {block} must stop a body");
        }
        // An id from a newer contract is solid too: the client draws what the
        // server sent rather than deciding a block it does not recognise is empty.
        assert!(is_solid(999));
    }

    #[test]
    fn air_water_and_cover_are_also_the_ids_you_can_see_through() {
        assert!(!is_opaque(AIR));
        assert!(!is_opaque(WATER));
        for block in PALETTE {
            if is_water(block) || is_cover(block) {
                assert!(!is_opaque(block), "block {block} must hide nothing");
                continue;
            }
            assert!(
                is_opaque(block),
                "block {block} must hide what is behind it"
            );
        }
        assert!(is_opaque(999), "an unknown id is drawn, not seen through");
    }

    #[test]
    fn ice_is_ordinary_ground_and_water_is_not() {
        // The pair #446 appended, and the whole of what separates them here: ice is
        // a block like stone, water is the exception every predicate above names.
        assert!(is_solid(ICE) && is_opaque(ICE));
        for block in PALETTE.into_iter().filter(|block| is_water(*block)) {
            assert!(!is_solid(block) && !is_opaque(block));
        }
    }

    #[test]
    fn every_non_water_palette_colour_is_distinct() {
        // Two ids that render the same colour would make the landscape unreadable
        // while every test still passed. Water ids deliberately share one material.
        let colours: Vec<[f32; 4]> = PALETTE
            .iter()
            .filter(|block| !is_water(**block))
            .map(|block| linear_rgba(*block))
            .collect();
        for (i, a) in colours.iter().enumerate() {
            for b in &colours[i + 1..] {
                assert_ne!(a, b, "two palette entries share a colour");
            }
        }
    }

    #[test]
    fn every_declared_block_id_has_a_colour() {
        let unknown = [UNKNOWN_LINEAR[0], UNKNOWN_LINEAR[1], UNKNOWN_LINEAR[2], 1.0];
        for block in 1..=FLOWER_BLUE {
            assert_ne!(
                linear_rgba(block),
                unknown,
                "block {block} falls through to the unknown colour"
            );
        }
    }

    #[test]
    fn an_id_this_build_does_not_know_is_loud_rather_than_plausible() {
        let unknown = linear_rgba(BlockId::MAX);
        assert_eq!(
            unknown,
            linear_rgba(AIR),
            "both fall back to the same swatch"
        );
        for known in PALETTE {
            assert_ne!(
                unknown,
                linear_rgba(known),
                "the fallback must not be mistakable for a real block"
            );
        }
    }

    #[test]
    fn the_water_family_is_the_only_part_not_fully_opaque() {
        assert!(
            (0.0..1.0).contains(&WATER_ALPHA),
            "water that is opaque, or absent, is not water"
        );
        for block in PALETTE.into_iter().filter(|block| is_water(*block)) {
            assert_eq!(linear_rgba(block)[3], WATER_ALPHA);
            assert_eq!(linear_rgba(block), linear_rgba(WATER));
        }
        for block in PALETTE.iter().chain(&[AIR, BlockId::MAX]) {
            if is_water(*block) {
                continue;
            }
            assert_eq!(
                linear_rgba(*block)[3],
                1.0,
                "block {block} must reach the framebuffer whole"
            );
        }
    }

    #[test]
    fn cover_is_exactly_the_three_flowers_and_stops_nothing() {
        // The seam the next cover block is added at, pinned as a set rather than as a
        // predicate: an id that starts answering `is_cover` without being listed here is
        // a block a body would walk through by accident.
        for block in [FLOWER_RED, FLOWER_YELLOW, FLOWER_BLUE] {
            assert!(is_cover(block));
            assert!(!is_solid(block), "cover {block} must stop no body");
            assert!(!is_opaque(block), "cover {block} must hide nothing");
            assert!(!is_water(block), "cover is not fluid; it is a third answer");
            assert_eq!(
                linear_rgba(block)[3],
                1.0,
                "cover reaches the framebuffer whole"
            );
        }
        for block in PALETTE
            .into_iter()
            .filter(|block| !matches!(*block, FLOWER_RED | FLOWER_YELLOW | FLOWER_BLUE))
            .chain([AIR, BlockId::MAX])
        {
            assert!(!is_cover(block), "block {block} is not cover");
        }
    }

    #[test]
    fn water_levels_and_currents_cover_the_whole_family() {
        assert_eq!(water_level(WATER), 8);
        for (block, level) in (WATER_FLOW1..=WATER_FLOW7).zip(1..=7) {
            assert!(is_water(block));
            assert_eq!(water_level(block), level);
            assert_eq!(current_of(block), (0, 0));
        }
        for (block, current) in [
            (WATER_CURRENT_XPOS, (1, 0)),
            (WATER_CURRENT_XNEG, (-1, 0)),
            (WATER_CURRENT_ZPOS, (0, 1)),
            (WATER_CURRENT_ZNEG, (0, -1)),
        ] {
            assert!(is_water(block));
            assert_eq!(water_level(block), 8);
            assert_eq!(current_of(block), current);
        }
        for block in [AIR, STONE, BUSH, FLOWER_RED, BlockId::MAX] {
            assert!(!is_water(block));
            assert_eq!(water_level(block), 0);
            assert_eq!(current_of(block), (0, 0));
        }
    }
}
