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
/// The one block a body moves *through*. Not solid, not opaque, and the only id in
/// the palette that is neither.
pub const WATER: BlockId = 12;
/// The lid on some of the water. Ordinary ground: solid, opaque, walked on.
pub const ICE: BlockId = 13;

/// Whether a block stops a body and can be aimed at.
///
/// The predicate everything outside the mesher asks — the aiming ray, the camera boom,
/// the store's `solid_at` — and the client's mirror of the server's `world.Solid`.
/// [`WATER`] is the one id that is *there* without stopping anything, so a ray passes
/// through it and the block behind it is what gets outlined.
pub fn is_solid(block: BlockId) -> bool {
    block != AIR && block != WATER
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
pub fn is_opaque(block: BlockId) -> bool {
    block != AIR && block != WATER
}

/// The palette in the order a reader wants to see it. Test-only: production code
/// asks [`linear_rgba`] about one block at a time.
#[cfg(test)]
pub const PALETTE: [BlockId; 13] = [
    STONE, DIRT, GRASS, SNOW, LOG, LEAVES, COAL_ORE, IRON_ORE, SAND, SANDSTONE, GRAVEL, WATER, ICE,
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

/// The colour of "this build has no colour for that id". `#C81E96`.
///
/// Magenta on purpose: a server one contract ahead sends a block this client has
/// never heard of, and the honest answer is to draw it loudly rather than guess a
/// plausible grey that hides the version skew.
const UNKNOWN_LINEAR: [f32; 3] = [0.577_580, 0.012_983, 0.304_987];

/// The linear RGBA a vertex of this block's faces carries.
///
/// Alpha is 1 for every id but [`WATER`], which carries [`WATER_ALPHA`]. The
/// renderer draws water in a pass of its own with a blending material, and this is
/// what that pass fades by.
pub fn linear_rgba(block: BlockId) -> [f32; 4] {
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
        // The one early return: every other id leaves this match with an opaque
        // alpha appended below, and water is the one that must not.
        WATER => {
            return [
                WATER_LINEAR[0],
                WATER_LINEAR[1],
                WATER_LINEAR[2],
                WATER_ALPHA,
            ];
        }
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
    const SRGB: [(&str, [u8; 3], [f32; 3]); 14] = [
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
    fn air_and_water_are_the_two_ids_a_body_moves_through() {
        assert!(!is_solid(AIR));
        assert!(!is_solid(WATER), "water is swum through, not walked into");
        for block in PALETTE {
            if block == WATER {
                continue;
            }
            assert!(is_solid(block), "block {block} must stop a body");
        }
        // An id from a newer contract is solid too: the client draws what the
        // server sent rather than deciding a block it does not recognise is empty.
        assert!(is_solid(999));
    }

    #[test]
    fn air_and_water_are_also_the_two_ids_you_can_see_through() {
        assert!(!is_opaque(AIR));
        assert!(!is_opaque(WATER));
        for block in PALETTE {
            if block == WATER {
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
        assert!(!is_solid(WATER) && !is_opaque(WATER));
    }

    #[test]
    fn every_palette_colour_is_distinct() {
        // Two ids that render the same colour would make the landscape unreadable
        // while every test still passed.
        let colours: Vec<[f32; 4]> = PALETTE.iter().map(|b| linear_rgba(*b)).collect();
        for (i, a) in colours.iter().enumerate() {
            for b in &colours[i + 1..] {
                assert_ne!(a, b, "two palette entries share a colour");
            }
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
    fn water_is_the_only_id_that_is_not_fully_opaque() {
        assert_eq!(linear_rgba(WATER)[3], WATER_ALPHA);
        assert!(
            (0.0..1.0).contains(&WATER_ALPHA),
            "water that is opaque, or absent, is not water"
        );
        for block in PALETTE.iter().chain(&[AIR, BlockId::MAX]) {
            if *block == WATER {
                continue;
            }
            assert_eq!(
                linear_rgba(*block)[3],
                1.0,
                "block {block} must reach the framebuffer whole"
            );
        }
    }
}
