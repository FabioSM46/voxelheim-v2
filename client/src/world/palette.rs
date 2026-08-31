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

// What a castle is built of, mirroring the server's `world.SmoothBlackStone` through
// `world.DarkGlass` (#680). All eight are ordinary ground here — solid, opaque, and
// swept as cubes — so no predicate above learns a new arm; what they carry that nothing
// else does is a **lightness ladder**, and it is the whole reason there are eight rather
// than three.
//
// There are no textures: a material is its colour, and greedy meshing merges faces
// across neighbours, so two materials a shade apart are one wall. Four of these are
// near-black and they are half a castle between them, so their sRGB values below are
// spread rather than crowded — `#17151C`, `#2B2733`, `#443E50`, `#665A78` — and
// [`SLATE_TILE`] at `#ABB2BC` is the pale roof the whole silhouette reads against.
/// Trim, thresholds and a hall floor: the darkest id in the palette.
pub const SMOOTH_BLACK_STONE: BlockId = 36;
/// Rough dark stone — a castle's plinth, footings and rubble.
pub const BASALT: BlockId = 37;
/// Dressed dark stone: the wall, and a quarter of the reference build on its own.
pub const BLACK_BRICK: BlockId = 38;
/// The same wall weathered. Lighter and greyer on purpose — mixed into [`BLACK_BRICK`]
/// it is what keeps a sixty-block face from reading as one slab of paint.
pub const BLACK_BRICK_WORN: BlockId = 39;
/// Pale roof stone: every roof and every spire, and the contrast the silhouette lives on.
pub const SLATE_TILE: BlockId = 40;
/// Dark beams, floorboards, doors and bridges.
pub const DARK_TIMBER: BlockId = 41;
/// Pale stripped timber: frames, rafters and scaffolding.
pub const PALE_TIMBER: BlockId = 42;
/// A window. **Opaque today, and the one id in this palette expected to change class**:
/// see-through glass needs a translucent pass the mesher does not have, so a pane reads
/// from outside as the dark hole a window is at any distance. When that pass arrives
/// [`is_opaque`] is the single function that has to learn about it, exactly as its doc
/// comment has promised since before this id existed.
pub const DARK_GLASS: BlockId = 43;

// Geometry and orientation live in the existing id; every variant uses [`SLATE_TILE`].
// That keeps chunk RLE, deltas and the wire unchanged.
pub const SLATE_SLAB_BOTTOM: BlockId = 44;
pub const SLATE_SLAB_TOP: BlockId = 45;
pub const SLATE_STAIR_NORTH_BOTTOM: BlockId = 46;
pub const SLATE_STAIR_EAST_BOTTOM: BlockId = 47;
pub const SLATE_STAIR_SOUTH_BOTTOM: BlockId = 48;
pub const SLATE_STAIR_WEST_BOTTOM: BlockId = 49;
pub const SLATE_STAIR_NORTH_TOP: BlockId = 50;
pub const SLATE_STAIR_EAST_TOP: BlockId = 51;
pub const SLATE_STAIR_SOUTH_TOP: BlockId = 52;
pub const SLATE_STAIR_WEST_TOP: BlockId = 53;

/// Geometry one block occupies inside its voxel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    Cube,
    Slab,
    Stair,
}

/// The vertical half a slab or stair is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeHalf {
    Bottom,
    Top,
}

/// The high horizontal half of a stair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeFacing {
    North,
    East,
    South,
    West,
}

/// Geometry and base material recovered from one block id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockShape {
    pub kind: ShapeKind,
    pub half: ShapeHalf,
    pub facing: ShapeFacing,
    pub material: BlockId,
}

/// One axis-aligned piece of a block shape, in half-block coordinates.
///
/// The server carries the same bounds as floats in local voxel coordinates. Keeping
/// this mirror on the exact `0..=2` half-grid makes every comparison integer while
/// still describing the only boundaries slabs and stairs use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockBounds {
    pub min: [u8; 3],
    pub max: [u8; 3],
}

const FULL_BLOCK_BOUNDS: BlockBounds = BlockBounds {
    min: [0, 0, 0],
    max: [2, 2, 2],
};

/// Decodes geometry and material; unknown ids fail closed as solid cubes.
pub const fn shape_of(block: BlockId) -> BlockShape {
    let mut shape = BlockShape {
        kind: ShapeKind::Cube,
        half: ShapeHalf::Bottom,
        facing: ShapeFacing::North,
        material: block,
    };
    match block {
        SLATE_SLAB_BOTTOM => {
            shape.kind = ShapeKind::Slab;
            shape.material = SLATE_TILE;
        }
        SLATE_SLAB_TOP => {
            shape.kind = ShapeKind::Slab;
            shape.half = ShapeHalf::Top;
            shape.material = SLATE_TILE;
        }
        SLATE_STAIR_NORTH_BOTTOM..=SLATE_STAIR_WEST_BOTTOM => {
            shape.kind = ShapeKind::Stair;
            shape.facing = match block - SLATE_STAIR_NORTH_BOTTOM {
                0 => ShapeFacing::North,
                1 => ShapeFacing::East,
                2 => ShapeFacing::South,
                _ => ShapeFacing::West,
            };
            shape.material = SLATE_TILE;
        }
        SLATE_STAIR_NORTH_TOP..=SLATE_STAIR_WEST_TOP => {
            shape.kind = ShapeKind::Stair;
            shape.half = ShapeHalf::Top;
            shape.facing = match block - SLATE_STAIR_NORTH_TOP {
                0 => ShapeFacing::North,
                1 => ShapeFacing::East,
                2 => ShapeFacing::South,
                _ => ShapeFacing::West,
            };
            shape.material = SLATE_TILE;
        }
        _ => {}
    }
    shape
}

/// Whether `block` is a solid slab/stair rather than a full cube or plant shape.
pub const fn is_architectural_shape(block: BlockId) -> bool {
    !matches!(shape_of(block).kind, ShapeKind::Cube)
}

/// The occupied pieces of a solid block, mirroring the server's
/// `world.CollisionBounds` exactly.
pub fn collision_bounds(block: BlockId) -> ([BlockBounds; 2], usize) {
    let mut bounds = [BlockBounds::default(); 2];
    if !is_solid(block) {
        return (bounds, 0);
    }

    let shape = shape_of(block);
    match shape.kind {
        ShapeKind::Cube => {
            bounds[0] = FULL_BLOCK_BOUNDS;
            (bounds, 1)
        }
        ShapeKind::Slab => {
            bounds[0] = if shape.half == ShapeHalf::Top {
                BlockBounds {
                    min: [0, 1, 0],
                    max: [2, 2, 2],
                }
            } else {
                BlockBounds {
                    min: [0, 0, 0],
                    max: [2, 1, 2],
                }
            };
            (bounds, 1)
        }
        ShapeKind::Stair => {
            bounds[0] = if shape.half == ShapeHalf::Top {
                BlockBounds {
                    min: [0, 1, 0],
                    max: [2, 2, 2],
                }
            } else {
                BlockBounds {
                    min: [0, 0, 0],
                    max: [2, 1, 2],
                }
            };
            bounds[1] = if shape.half == ShapeHalf::Top {
                BlockBounds {
                    min: [0, 0, 0],
                    max: [2, 1, 2],
                }
            } else {
                BlockBounds {
                    min: [0, 1, 0],
                    max: [2, 2, 2],
                }
            };
            match shape.facing {
                ShapeFacing::North => bounds[1].max[2] = 1,
                ShapeFacing::East => bounds[1].min[0] = 1,
                ShapeFacing::South => bounds[1].min[2] = 1,
                ShapeFacing::West => bounds[1].max[0] = 1,
            }
            (bounds, 2)
        }
    }
}

/// Whether one of the eight half-block cells is occupied by `block`.
pub fn occupies_half(block: BlockId, half: [u8; 3]) -> bool {
    if half.iter().any(|coordinate| *coordinate >= 2) {
        return false;
    }
    let (bounds, count) = collision_bounds(block);
    bounds[..count].iter().any(|bounds| {
        (0..3).all(|axis| half[axis] >= bounds.min[axis] && half[axis] < bounds.max[axis])
    })
}

/// Which of the four half-cells on one voxel face hide the chunk across it.
///
/// Bits are ordered with the first in-plane axis varying fastest. This is the
/// border signature `ChunkStore` compares when a neighbour revision changes: a
/// bottom slab becoming a top slab must remesh the chunk beside it even though both
/// answers are broadly "opaque".
pub fn opaque_face_mask(block: BlockId, axis: usize, positive: bool) -> u8 {
    if !is_opaque(block) || axis >= 3 {
        return 0;
    }
    let u = (axis + 1) % 3;
    let v = (axis + 2) % 3;
    let mut mask = 0;
    for j in 0..2 {
        for i in 0..2 {
            let mut half = [0u8; 3];
            half[axis] = u8::from(positive);
            half[u] = i;
            half[v] = j;
            if occupies_half(block, half) {
                mask |= 1 << (j * 2 + i);
            }
        }
    }
    mask
}

/// Whether the ordinary greedy cube sweep owns this block.
pub fn is_greedy_opaque(block: BlockId) -> bool {
    is_opaque(block) && !is_architectural_shape(block)
}

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

/// Whether the mesher grows this block a shape of its own inside its voxel rather than
/// sweeping it as a cube.
///
/// Exactly [`is_cover`] plus [`BUSH`], and it is a **third** question about a block
/// rather than a synonym for either of the two above it. Cover answers what a body does
/// with a voxel — mirrored from the server's `world.Cover` — and a bush is `world.Solid`
/// there, so it stops a body while a flower does not. What the two share is only that
/// neither is a cube: [`super::mesher::build_cover`] builds a stem, a corolla and leaves
/// for one and a clump of foliage for the other, and the sweep never sees either.
///
/// **The consequence is [`is_opaque`], and it is the reason this predicate has to exist
/// at all.** A shape does not fill the voxel the sweep would have culled against, so the
/// grass under a bush keeps its top face and the dirt beside one keeps its side face —
/// otherwise every gap between two clumps of foliage would look through the bush onto a
/// face nobody drew. It says nothing about solidity: `is_solid` still reads [`is_cover`],
/// so a bush stops a body exactly as it did before it was drawn as one.
pub fn is_shaped(block: BlockId) -> bool {
    is_cover(block) || block == BUSH
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

/// Whether water in `block` supplies the adjacent voxel at `(x, z)` from itself.
///
/// This mirrors the server's `WaterFeedsToward`: still and flowing water may feed
/// every cardinal side, while a current source feeds only the side encoded by its id.
pub fn water_feeds_toward(block: BlockId, x: i8, z: i8) -> bool {
    if !is_water(block) {
        return false;
    }
    let current = current_of(block);
    current == (0, 0) || current == (x, z)
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
    is_architectural_shape(block) || (block != AIR && !is_water(block) && !is_cover(block))
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
/// [`is_shaped`] is not opaque, and that is the whole of what the two masks need to learn
/// about a plant: the grass under a flower or a bush keeps its top face, and the water
/// beside one keeps its surface, because both mask arms already read "see-through" rather
/// than "air". A bush is the id where that is a rendering answer and not a physical one —
/// it still stops a body, and `is_solid` above is where that is said.
pub fn is_opaque(block: BlockId) -> bool {
    block != AIR && !is_water(block) && !is_shaped(block)
}

/// The palette in the order a reader wants to see it. Test-only: production code
/// asks [`linear_rgba`] about one block at a time.
#[cfg(test)]
pub const PALETTE: [BlockId; 53] = [
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
    SMOOTH_BLACK_STONE,
    BASALT,
    BLACK_BRICK,
    BLACK_BRICK_WORN,
    SLATE_TILE,
    DARK_TIMBER,
    PALE_TIMBER,
    DARK_GLASS,
    SLATE_SLAB_BOTTOM,
    SLATE_SLAB_TOP,
    SLATE_STAIR_NORTH_BOTTOM,
    SLATE_STAIR_EAST_BOTTOM,
    SLATE_STAIR_SOUTH_BOTTOM,
    SLATE_STAIR_WEST_BOTTOM,
    SLATE_STAIR_NORTH_TOP,
    SLATE_STAIR_EAST_TOP,
    SLATE_STAIR_SOUTH_TOP,
    SLATE_STAIR_WEST_TOP,
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

/// The darkest thing in the world, and a castle's trim rather than its wall: a line of
/// it under a sill or along a threshold is what gives a face an edge. `#17151C`.
const SMOOTH_BLACK_STONE_LINEAR: [f32; 3] = [0.008_568, 0.007_499, 0.011_612];

/// Rough dark stone, violet where [`COAL_ORE_LINEAR`] is blue — the two are close in
/// lightness and never adjacent, since one is a castle's plinth and the other is a seam
/// two hundred blocks under it. `#2B2733`.
const BASALT_LINEAR: [f32; 3] = [0.024_158, 0.020_289, 0.033_105];

/// The dressed wall. `#443E50`.
const BLACK_BRICK_LINEAR: [f32; 3] = [0.057_805, 0.048_172, 0.080_220];

/// The weathered wall: two steps lighter than [`BLACK_BRICK_LINEAR`] and further into
/// violet, so a mixed face has a grain instead of a tone. `#665A78`.
const BLACK_BRICK_WORN_LINEAR: [f32; 3] = [0.132_868, 0.102_242, 0.187_821];

/// Pale cool roof stone. Lighter than [`COBBLESTONE_LINEAR`] and far lighter than any of
/// the four blacks, because a roof read against a dark wall at distance is the one thing
/// a castle is recognised by. `#ABB2BC`.
const SLATE_TILE_LINEAR: [f32; 3] = [0.407_240, 0.445_201, 0.502_886];

/// Dark, red-brown sawn timber — darker than [`LOG_LINEAR`], which is the conifer bark it
/// must not be mistaken for. `#3E2A1C`.
const DARK_TIMBER_LINEAR: [f32; 3] = [0.048_172, 0.023_153, 0.011_612];

/// Stripped pale timber: lighter and much less saturated than [`SAND_LINEAR`], which is
/// the only other pale warm tone in the palette. `#D2C4AE`.
const PALE_TIMBER_LINEAR: [f32; 3] = [0.644_480, 0.552_011, 0.423_268];

/// A window pane: near-black, and cold where [`SMOOTH_BLACK_STONE_LINEAR`] beside it in a
/// wall is warm. Hue is the only thing separating the two, because both have to be dark.
/// `#16202E`.
const DARK_GLASS_LINEAR: [f32; 3] = [0.008_023, 0.014_444, 0.027_321];

// Ten geometry ids, one material. Separate aliases keep the id-to-colour parity
// check exhaustive without pretending each orientation owns a distinct swatch.
const SLATE_SLAB_BOTTOM_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_SLAB_TOP_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_STAIR_NORTH_BOTTOM_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_STAIR_EAST_BOTTOM_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_STAIR_SOUTH_BOTTOM_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_STAIR_WEST_BOTTOM_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_STAIR_NORTH_TOP_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_STAIR_EAST_TOP_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_STAIR_SOUTH_TOP_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;
const SLATE_STAIR_WEST_TOP_LINEAR: [f32; 3] = SLATE_TILE_LINEAR;

/// The stem every flower stands on, whatever colour its head is. `#3E6B2E`.
///
/// Darker and yellower than [`GRASS_LINEAR`] so a stem is a shape against the ground
/// rather than a smear of it. **Not a block colour**: no id renders as this, so
/// [`linear_rgba`] never returns it and the palette's distinctness test never sees it.
/// [`super::mesher::build_cover`] asks for it directly, once per stem quad.
pub const STEM_LINEAR: [f32; 3] = [0.048_172, 0.147_027, 0.027_321];

/// The pair of leaves partway up a flower's stem. `#4C8438`.
///
/// A fresher green than [`STEM_LINEAR`], so a leaf is a blade catching the light rather
/// than a wider stem. **Not a block colour**, exactly as the stem is not.
pub const LEAF_LINEAR: [f32; 3] = [0.072_272, 0.230_740, 0.039_546];

/// The eye at the middle of a corolla, where the petals meet. `#7A5A1E`.
///
/// Dark and warm so it reads against all three petal colours — a lighter centre would
/// vanish inside the yellow flower, which is the one the corolla has least contrast with.
/// **Not a block colour.**
pub const FLOWER_CENTRE_LINEAR: [f32; 3] = [0.194_618, 0.102_242, 0.012_983];

/// The sunlit top of a bush's foliage. `#56945A`.
///
/// Lighter than [`BUSH_LINEAR`], which is what gives a clump of bushes a top and a
/// shaded underside instead of one flat tone across the whole slab they used to be.
/// **Not a block colour**: [`linear_rgba`] answers [`BUSH_LINEAR`] for [`BUSH`], and this
/// is the second tone the mesher reaches for directly.
pub const BUSH_CROWN_LINEAR: [f32; 3] = [0.093_059, 0.296_138, 0.102_242];

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
        // A cover block's colour is its **head**. The stem is [`STEM_LINEAR`], which the
        // mesher asks for by name, because a flower is two colours in one voxel and this
        // function answers per id.
        FLOWER_RED => FLOWER_RED_LINEAR,
        FLOWER_YELLOW => FLOWER_YELLOW_LINEAR,
        FLOWER_BLUE => FLOWER_BLUE_LINEAR,
        SMOOTH_BLACK_STONE => SMOOTH_BLACK_STONE_LINEAR,
        BASALT => BASALT_LINEAR,
        BLACK_BRICK => BLACK_BRICK_LINEAR,
        BLACK_BRICK_WORN => BLACK_BRICK_WORN_LINEAR,
        SLATE_TILE => SLATE_TILE_LINEAR,
        DARK_TIMBER => DARK_TIMBER_LINEAR,
        PALE_TIMBER => PALE_TIMBER_LINEAR,
        DARK_GLASS => DARK_GLASS_LINEAR,
        SLATE_SLAB_BOTTOM => SLATE_SLAB_BOTTOM_LINEAR,
        SLATE_SLAB_TOP => SLATE_SLAB_TOP_LINEAR,
        SLATE_STAIR_NORTH_BOTTOM => SLATE_STAIR_NORTH_BOTTOM_LINEAR,
        SLATE_STAIR_EAST_BOTTOM => SLATE_STAIR_EAST_BOTTOM_LINEAR,
        SLATE_STAIR_SOUTH_BOTTOM => SLATE_STAIR_SOUTH_BOTTOM_LINEAR,
        SLATE_STAIR_WEST_BOTTOM => SLATE_STAIR_WEST_BOTTOM_LINEAR,
        SLATE_STAIR_NORTH_TOP => SLATE_STAIR_NORTH_TOP_LINEAR,
        SLATE_STAIR_EAST_TOP => SLATE_STAIR_EAST_TOP_LINEAR,
        SLATE_STAIR_SOUTH_TOP => SLATE_STAIR_SOUTH_TOP_LINEAR,
        SLATE_STAIR_WEST_TOP => SLATE_STAIR_WEST_TOP_LINEAR,
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
    const SRGB: [(&str, [u8; 3], [f32; 3]); 29] = [
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
        ("stem", [0x3E, 0x6B, 0x2E], STEM_LINEAR),
        ("leaf", [0x4C, 0x84, 0x38], LEAF_LINEAR),
        ("flower centre", [0x7A, 0x5A, 0x1E], FLOWER_CENTRE_LINEAR),
        ("bush crown", [0x56, 0x94, 0x5A], BUSH_CROWN_LINEAR),
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
    fn air_water_and_the_shaped_plants_are_the_ids_you_can_see_through() {
        assert!(!is_opaque(AIR));
        assert!(!is_opaque(WATER));
        for block in PALETTE {
            if is_water(block) || is_shaped(block) {
                assert!(!is_opaque(block), "block {block} must hide nothing");
                continue;
            }
            assert!(
                is_opaque(block),
                "block {block} must hide what is behind it"
            );
        }
        assert!(is_opaque(999), "an unknown id is drawn, not seen through");
        // The bush is where opacity and solidity part company, and #634 is what parted
        // them: it is drawn as a clump of foliage with gaps, so the ground under it has
        // to keep the faces the sweep used to cull, and it still stops a body.
        assert!(!is_opaque(BUSH), "a bush is foliage with gaps in it");
        assert!(is_solid(BUSH), "and it still stops a body");
        assert!(
            !is_cover(BUSH),
            "which is what keeps it out of the cover family"
        );
    }

    #[test]
    fn the_shaped_plants_are_the_bush_and_the_three_flowers() {
        // The set the mesher grows geometry for, pinned the way `is_cover` is: an id
        // that starts answering `is_shaped` without being listed here is a block that
        // silently stopped hiding what is behind it.
        for block in [BUSH, FLOWER_RED, FLOWER_YELLOW, FLOWER_BLUE] {
            assert!(is_shaped(block));
        }
        for block in PALETTE
            .into_iter()
            .filter(|block| !matches!(*block, BUSH | FLOWER_RED | FLOWER_YELLOW | FLOWER_BLUE))
            .chain([AIR, BlockId::MAX])
        {
            assert!(!is_shaped(block), "block {block} is swept as a cube");
        }
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
    fn every_non_water_material_colour_is_distinct() {
        // Two materials that render the same colour would make the landscape unreadable
        // while every test still passed. Water ids and slate's geometry variants
        // deliberately share one material each.
        let materials: Vec<(BlockId, [f32; 4])> = PALETTE
            .iter()
            .filter(|block| !is_water(**block))
            .map(|block| (shape_of(*block).material, linear_rgba(*block)))
            .collect();
        for (i, (a_material, a_colour)) in materials.iter().enumerate() {
            for (b_material, b_colour) in &materials[i + 1..] {
                if a_material == b_material {
                    assert_eq!(a_colour, b_colour, "one material has two colours");
                } else {
                    assert_ne!(a_colour, b_colour, "two materials share a colour");
                }
            }
        }
    }

    #[test]
    fn every_declared_block_id_has_a_colour() {
        let unknown = [UNKNOWN_LINEAR[0], UNKNOWN_LINEAR[1], UNKNOWN_LINEAR[2], 1.0];
        for block in 1..=SLATE_STAIR_WEST_TOP {
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
    fn slate_shapes_carry_every_orientation_and_one_material() {
        let blocks = [
            SLATE_SLAB_BOTTOM,
            SLATE_SLAB_TOP,
            SLATE_STAIR_NORTH_BOTTOM,
            SLATE_STAIR_EAST_BOTTOM,
            SLATE_STAIR_SOUTH_BOTTOM,
            SLATE_STAIR_WEST_BOTTOM,
            SLATE_STAIR_NORTH_TOP,
            SLATE_STAIR_EAST_TOP,
            SLATE_STAIR_SOUTH_TOP,
            SLATE_STAIR_WEST_TOP,
        ];
        let facings = [
            ShapeFacing::North,
            ShapeFacing::North,
            ShapeFacing::North,
            ShapeFacing::East,
            ShapeFacing::South,
            ShapeFacing::West,
            ShapeFacing::North,
            ShapeFacing::East,
            ShapeFacing::South,
            ShapeFacing::West,
        ];
        for (offset, block) in blocks.into_iter().enumerate() {
            assert_eq!(block, 44 + offset as BlockId);
            let kind = if offset < 2 {
                ShapeKind::Slab
            } else {
                ShapeKind::Stair
            };
            let half = if offset == 1 || offset >= 6 {
                ShapeHalf::Top
            } else {
                ShapeHalf::Bottom
            };
            assert_eq!(
                shape_of(block),
                BlockShape {
                    kind,
                    half,
                    facing: facings[offset],
                    material: SLATE_TILE,
                }
            );
            assert!(is_architectural_shape(block));
            assert!(is_solid(block));
            assert_eq!(linear_rgba(block), linear_rgba(SLATE_TILE));
        }

        for block in [AIR, STONE, SLATE_TILE, BlockId::MAX] {
            assert!(!is_architectural_shape(block));
            assert_eq!(
                shape_of(block),
                BlockShape {
                    kind: ShapeKind::Cube,
                    half: ShapeHalf::Bottom,
                    facing: ShapeFacing::North,
                    material: block,
                }
            );
        }
    }

    #[test]
    fn slate_shape_bounds_are_the_servers_half_block_collision_boxes() {
        let lower = BlockBounds {
            min: [0, 0, 0],
            max: [2, 1, 2],
        };
        let upper = BlockBounds {
            min: [0, 1, 0],
            max: [2, 2, 2],
        };
        assert_eq!(
            collision_bounds(SLATE_SLAB_BOTTOM),
            ([lower, BlockBounds::default()], 1)
        );
        assert_eq!(
            collision_bounds(SLATE_SLAB_TOP),
            ([upper, BlockBounds::default()], 1)
        );

        let directional = [
            BlockBounds {
                min: [0, 0, 0],
                max: [2, 2, 1],
            },
            BlockBounds {
                min: [1, 0, 0],
                max: [2, 2, 2],
            },
            BlockBounds {
                min: [0, 0, 1],
                max: [2, 2, 2],
            },
            BlockBounds {
                min: [0, 0, 0],
                max: [1, 2, 2],
            },
        ];
        for (offset, direction) in directional.into_iter().enumerate() {
            let bottom = SLATE_STAIR_NORTH_BOTTOM + offset as BlockId;
            let top = SLATE_STAIR_NORTH_TOP + offset as BlockId;
            let mut bottom_high = direction;
            bottom_high.min[1] = 1;
            let mut top_low = direction;
            top_low.max[1] = 1;
            assert_eq!(collision_bounds(bottom), ([lower, bottom_high], 2));
            assert_eq!(collision_bounds(top), ([upper, top_low], 2));
        }

        assert_eq!(collision_bounds(AIR).1, 0);
        assert_eq!(collision_bounds(FLOWER_RED).1, 0);
        assert_eq!(
            collision_bounds(BlockId::MAX),
            ([FULL_BLOCK_BOUNDS, BlockBounds::default()], 1)
        );
    }

    #[test]
    fn face_masks_distinguish_the_halves_and_orientations_a_neighbour_mesh_reads() {
        assert_eq!(opaque_face_mask(STONE, 0, false), 0b1111);
        assert_eq!(opaque_face_mask(FLOWER_RED, 0, false), 0);
        assert_eq!(opaque_face_mask(SLATE_SLAB_BOTTOM, 0, false), 0b0101);
        assert_eq!(opaque_face_mask(SLATE_SLAB_TOP, 0, false), 0b1010);
        assert_eq!(opaque_face_mask(SLATE_SLAB_BOTTOM, 1, false), 0b1111);
        assert_eq!(opaque_face_mask(SLATE_SLAB_BOTTOM, 1, true), 0);
        assert_ne!(
            opaque_face_mask(SLATE_STAIR_NORTH_BOTTOM, 0, false),
            opaque_face_mask(SLATE_STAIR_SOUTH_BOTTOM, 0, false)
        );
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
        // The stem, the leaf, the corolla's eye and the bush's sunlit crown are colours
        // and not blocks: nothing renders as one through this function, which is what
        // keeps all four out of the distinctness test above.
        for tone in [
            STEM_LINEAR,
            LEAF_LINEAR,
            FLOWER_CENTRE_LINEAR,
            BUSH_CROWN_LINEAR,
        ] {
            let rgba = [tone[0], tone[1], tone[2], 1.0];
            for block in PALETTE.into_iter().chain([AIR, BlockId::MAX]) {
                assert_ne!(linear_rgba(block), rgba);
            }
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

            for toward in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                assert_eq!(
                    water_feeds_toward(block, toward.0, toward.1),
                    current == toward,
                    "current {block} feeding {toward:?}"
                );
            }
        }
        for block in [WATER, WATER_FLOW1, WATER_FLOW7] {
            for (x, z) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                assert!(water_feeds_toward(block, x, z));
            }
        }
        for block in [AIR, STONE, BUSH, FLOWER_RED, BlockId::MAX] {
            assert!(!is_water(block));
            assert_eq!(water_level(block), 0);
            assert_eq!(current_of(block), (0, 0));
            assert!(!water_feeds_toward(block, 1, 0));
        }
    }
}
