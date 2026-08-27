//! The generated surface one material wears, and the field both its colour and its
//! geometry are read from.
//!
//! **Why an asset rather than a per-vertex patina** is recorded in `client/AGENTS.md`,
//! under the texture rule this module ended. The short of it: a patina would have to be
//! regenerated in every mesh at every scale and `ui/icon.rs` has no vertices to tint, so
//! the four surfaces that draw one item could never agree by construction. A shared handle
//! makes that agreement *identity*.
//!
//! **One image, and the neutral band is why that is enough.** Every mesh drawn by a
//! material carrying a livery samples this image, the fist and the wrist included. Row 0 is
//! pure white — identity for a multiplier — and [`neutral_uv`] is where all of that
//! geometry is pointed, so an un-liveried item is bit-for-bit the item it was before this
//! module existed.
//!
//! **The field decides the colour and the geometry, and it is one function.** Corrosion
//! eats metal; it does not sit on top of it. [`field`] answers how strong the livery is at
//! a point in `(around, along)` space; the pixels below are that answer as a multiplier,
//! and `hands.rs` displaces the blade's vertices inward by the same answer. A texture that
//! disagreed with the surface under it would be paint.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::items::Livery;

/// How many texels the livery carries around a blade's perimeter.
///
/// **More than the mesh has vertices around the same perimeter, and meant to be.** The
/// subdivision in `hands.rs` decides how finely the silhouette can pit; this decides how
/// finely the colour can vary, and the second is cheap where the first is not.
const LIVERY_WIDTH: u32 = 64;

/// How many texels the livery carries along a blade, the neutral band included.
const LIVERY_HEIGHT: u32 = 64;

/// How many rows at the top of the image are the neutral band.
///
/// One is enough because the sampler is [`ImageFilterMode::Nearest`]: nothing bleeds
/// between rows, so a single white row cannot be contaminated by the rust beneath it. Under
/// linear filtering this would have to be wider than the widest filter footprint, which is
/// a number nobody can write down for a mip chain.
const NEUTRAL_ROWS: u32 = 1;

/// The seed the livery is generated from.
///
/// **Deterministic, so the same sword looks the same every run** — a blade whose freckles
/// moved between sessions would be the one thing about it a player could not learn. The
/// same number `hands.rs` scattered the fourteen boxes from.
const RUST_SEED: u32 = 0x5EED_0204;

/// How much darker rust is than the iron it eats into.
///
/// **A multiplier, not a colour**, and that is what keeps `player/items.rs` the one answer
/// to which colour an item presents as. The image is white — identity — everywhere the
/// field is zero, so the base that comes through is whatever that table says. Change the
/// sword's item colour and the rust follows it, because it is a shade *of* it.
///
/// Warm and dark: red kept, green and blue pulled down, which turns a pale iron into oxide
/// rather than into grey. Stored in a **linear** texture rather than an sRGB one, so these
/// are the numbers the shader multiplies by — the semantics the vertex colours it replaced
/// had.
const RUST_TINT: [f32; 3] = [0.72, 0.38, 0.22];

/// How many freckles the rust field carries.
///
/// **Several small ones rather than three large ones**, which is the difference between
/// oxide and damage: rust takes hold in freckles across a blade, and three patches at fixed
/// heights read as somebody having hit it with something. The count the boxes this replaced
/// settled on, for the same reason.
const RUST_PATCHES: u32 = 14;

/// The radius of one freckle in the field's own `(around, along)` space, before [`scatter`]
/// varies it down.
///
/// The two axes are not the same length in metres — the perimeter is about 84 mm and the
/// blade about 79 mm — so a circle here is very nearly a circle on the steel, which is why
/// one radius serves both axes.
const RUST_PATCH_RADIUS: f32 = 0.085;

/// How much of each end of the blade stays clear of rust, as a fraction of its length.
///
/// **Wider than the boxes' 5%, and the point taper is why.** A freckle is placed by its
/// centre and spreads by its radius, so anything under `RUST_PATCH_RADIUS` would let one
/// reach the tip. Twelve per cent keeps the whole field off the point and out of the guard.
const RUST_MARGIN: f32 = 0.12;

/// A deterministic value in `0.0..1.0` for one freckle and one of its dimensions.
///
/// **A seeded hash rather than a crate**: `client/AGENTS.md` is explicit about the
/// dependency budget, and an integer hash is reproducible on every platform, which is what
/// [`RUST_SEED`]'s promise of the same sword every run actually requires.
fn scatter(mark: u32, channel: u32) -> f32 {
    let mut bits = mark
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(channel.wrapping_mul(0x85EB_CA6B))
        ^ RUST_SEED;
    bits ^= bits >> 16;
    bits = bits.wrapping_mul(0x7FEB_352D);
    bits ^= bits >> 15;
    bits = bits.wrapping_mul(0x846C_A68B);
    bits ^= bits >> 16;
    // The top 24 bits over their own range: every value of that width is exactly
    // representable in an f32, so the division is the only rounding anywhere in here.
    (bits >> 8) as f32 / 16_777_216.0
}

/// How strong one livery is at a point on the surface it dresses, in `0.0..=1.0`.
///
/// `around` runs the perimeter and **wraps**; `along` runs from the guard to the tip and
/// does not. Both are read by the pixels below and by the blade's own vertices, which is
/// what makes the pitting and the oxide the same fact rather than two that agree.
pub(super) fn field(livery: Livery, around: f32, along: f32) -> f32 {
    match livery {
        Livery::Rust => rust_field(around, along),
    }
}

fn rust_field(around: f32, along: f32) -> f32 {
    if !(RUST_MARGIN..=1.0 - RUST_MARGIN).contains(&along) {
        return 0.0;
    }
    let mut strongest: f32 = 0.0;
    for patch in 0..RUST_PATCHES {
        // **One freckle per stratum of the blade, jittered inside its own** — rather than
        // fourteen independent draws over the whole length. Fourteen samples of a hash
        // clump: the boxes this replaced left the top third and the bottom tenth bare and
        // put nine marks in the middle, which reads as a band rather than as weathering.
        // Stratifying makes *spread over the blade* a property of the placement instead of
        // a hope about the seed, and the jitter is what keeps it from being a row.
        let stratum = (patch as f32 + scatter(patch, 1)) / RUST_PATCHES as f32;
        let centre_along = RUST_MARGIN + stratum * (1.0 - 2.0 * RUST_MARGIN);
        let centre_around = scatter(patch, 2);
        let radius = RUST_PATCH_RADIUS * (0.5 + 0.5 * scatter(patch, 0));

        // The short way round. The perimeter is a closed loop, so a freckle near the seam
        // must reach across it — otherwise the one place the blade's own geometry has no
        // edge would be the one place the rust always stops.
        let round = (around - centre_around).abs();
        let across = round.min(1.0 - round);
        let along_gap = along - centre_along;
        let distance = (across * across + along_gap * along_gap).sqrt() / radius;
        // Smooth rather than a disc: a hard-edged patch in a texture is the same defect as
        // a box on the surface, one dimension down.
        strongest = strongest.max((1.0 - distance * distance).max(0.0));
    }
    strongest.clamp(0.0, 1.0)
}

/// The texture coordinate every vertex that wears no livery carries.
///
/// The centre of a texel in the neutral band, so the nearest-neighbour sampler lands on
/// white exactly and no arithmetic anywhere else has to know the band exists.
pub(super) fn neutral_uv() -> [f32; 2] {
    [0.5, 0.5 / LIVERY_HEIGHT as f32]
}

/// The texture coordinate for a point on a liveried blade.
///
/// `along` is squeezed past the neutral band rather than starting at zero, which is the one
/// piece of arithmetic the band costs.
pub(super) fn blade_uv(around: f32, along: f32) -> [f32; 2] {
    let rows = (LIVERY_HEIGHT - NEUTRAL_ROWS) as f32;
    [
        around,
        (NEUTRAL_ROWS as f32 + along * rows) / LIVERY_HEIGHT as f32,
    ]
}

/// How strong a livery is at a texture coordinate a mesh actually carries.
///
/// The inverse of [`blade_uv`], and test-only: it lets a test in `hands.rs` ask what rust a
/// blade's own coordinates reach without that module learning this image's layout. A
/// renderer never needs it — the sampler does this in hardware.
#[cfg(test)]
pub(super) fn strength_at(livery: Livery, uv: [f32; 2]) -> f32 {
    let [around, v] = uv;
    let rows = (LIVERY_HEIGHT - NEUTRAL_ROWS) as f32;
    let along = (v * LIVERY_HEIGHT as f32 - NEUTRAL_ROWS as f32) / rows;
    field(livery, around, along)
}

/// One livery, as the pixels a material samples.
fn image_for(livery: Livery) -> Image {
    let mut data = Vec::with_capacity((LIVERY_WIDTH * LIVERY_HEIGHT * 4) as usize);
    for row in 0..LIVERY_HEIGHT {
        for column in 0..LIVERY_WIDTH {
            let strength = if row < NEUTRAL_ROWS {
                0.0
            } else {
                let around = (column as f32 + 0.5) / LIVERY_WIDTH as f32;
                let along =
                    ((row - NEUTRAL_ROWS) as f32 + 0.5) / (LIVERY_HEIGHT - NEUTRAL_ROWS) as f32;
                field(livery, around, along)
            };
            for channel in RUST_TINT {
                let value = 1.0 + (channel - 1.0) * strength;
                data.push((value * 255.0).round() as u8);
            }
            data.push(u8::MAX);
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: LIVERY_WIDTH,
            height: LIVERY_HEIGHT,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        // **Linear rather than sRGB, and it is the multiplier that decides that.** These
        // texels stand where vertex colours stood, and a vertex colour is linear; storing
        // them in an sRGB texture would have the sampler decode them a second time and the
        // rust would come out darker than the number it is named after.
        TextureFormat::Rgba8Unorm,
        // Both worlds: the render world draws it and the main world is where the tests —
        // and `hands.rs`'s own field reads — can see it.
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        // **The perimeter wraps and the blade does not.** A blade's `around` reaches
        // exactly 1.0 at the seam, which is the same place as 0.0; repeating is what makes
        // those two the same texel instead of the first and the last.
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::ClampToEdge,
        // **Nearest, and this is not a taste.** The vertices are displaced by [`field`] and
        // the texels are [`field`]; interpolating the colour and not the geometry would
        // make them disagree exactly where this module insists they agree. A generated
        // field this size has no detail to interpolate toward either.
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        ..default()
    });
    image
}

/// The one handle each livery is drawn from.
///
/// **One asset per livery, minted once and handed out**, which is the whole point of
/// choosing an image over a patina: the surfaces that draw an item agree because they hold
/// the same handle, not because somebody kept two generators in step.
#[derive(Resource, Debug, Clone)]
pub(super) struct Liveries {
    rust: Handle<Image>,
}

impl Liveries {
    /// The image any material that may draw a liveried item must carry.
    ///
    /// **One material can serve liveried and un-liveried geometry at once**, because the
    /// neutral band is in the same image — so a hand holding an iron sword and a hand
    /// holding a rusty one are one draw with one material, exactly as they were before.
    /// With more than one livery this becomes the choice `player/items.rs` names; today
    /// there is one image and every caller wants it.
    pub(super) fn material_image(&self) -> Handle<Image> {
        self.rust.clone()
    }
}

impl FromWorld for Liveries {
    fn from_world(world: &mut World) -> Self {
        let mut images = world.resource_mut::<Assets<Image>>();
        Self {
            rust: images.add(image_for(Livery::Rust)),
        }
    }
}

/// Generates every livery and publishes the handles.
///
/// **`init_resource` rather than a `Startup` system**: `FromWorld` runs while the caller is
/// being built, so the handles exist before the first `Startup` system asks for one and
/// there is no ordering for anybody to get wrong — a view model spawned before its image
/// would draw untextured steel and look exactly like a livery nobody wrote.
///
/// **A function rather than a `Plugin`, and that is not a style choice.** Bevy panics when
/// one plugin is added twice, and the surfaces that draw a liveried item are about to be
/// four modules rather than one, none of which can know whether another has already asked.
/// Both calls below are idempotent, so every caller may say what it needs.
///
/// `init_asset::<Image>` is for the headless tests — a no-op under `ImagePlugin`, and the
/// same defence `HandsPlugin` keeps for the four resources it does not own.
pub(super) fn register(app: &mut App) {
    app.init_asset::<Image>().init_resource::<Liveries>();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes one texel of a generated livery carries.
    fn texel(image: &Image, column: u32, row: u32) -> [u8; 4] {
        let data = image
            .data
            .as_ref()
            .expect("a generated livery carries data");
        let at = ((row * LIVERY_WIDTH + column) * 4) as usize;
        [data[at], data[at + 1], data[at + 2], data[at + 3]]
    }

    /// **The same livery every run**, which is what a seeded generator buys over a random
    /// one.
    ///
    /// Named texels against fixed values rather than a hash of the buffer: a hash says
    /// *that* something moved, these say *what*, and the second is what somebody re-tuning
    /// [`RUST_PATCH_RADIUS`] needs. The four are the neutral band, a texel inside the margin
    /// the field must leave clear, and two the rust reaches.
    #[test]
    fn the_rust_livery_is_generated_the_same_way_every_time() {
        let image = image_for(Livery::Rust);

        assert_eq!(
            texel(&image, 0, 0),
            [255, 255, 255, 255],
            "the neutral band is not white, so every un-liveried item is tinted by it"
        );
        assert_eq!(
            texel(&image, 32, 1),
            [255, 255, 255, 255],
            "the guard end of the blade carries rust, so the margin is not being kept"
        );
        // The deepest texel the field reaches, which is all but exactly [`RUST_TINT`] —
        // the freckle's own centre falls between texel centres, so it is one part in 255
        // short of it rather than equal to it. That is worth pinning as the number it is:
        // a livery that started clamping, or one whose tint drifted, changes it.
        assert_eq!(texel(&image, 58, 10), [184, 98, 58, 255]);
        // And one in the shoulder of a freckle, so the falloff is pinned as well as the
        // peak: a field that became a hard-edged disc would keep the value above and lose
        // this one.
        assert_eq!(texel(&image, 40, 30), [207, 148, 121, 255]);

        // And the whole buffer, so a change that misses all four named texels still has to
        // be looked at rather than merely passing.
        let rusted = (0..LIVERY_HEIGHT)
            .flat_map(|row| (0..LIVERY_WIDTH).map(move |column| (column, row)))
            .filter(|&(column, row)| texel(&image, column, row) != [255, 255, 255, 255])
            .count();
        assert_eq!(
            rusted,
            709,
            "the rust covers {rusted} texels of {}, which is not what it covered",
            LIVERY_WIDTH * LIVERY_HEIGHT
        );
    }

    /// **The field leaves both ends of the blade clear**, which is what stops a freckle
    /// blunting the point or disappearing into the guard.
    #[test]
    fn the_rust_field_keeps_off_the_point_and_out_of_the_guard() {
        for step in 0..=64 {
            let around = step as f32 / 64.0;
            for along in [0.0, RUST_MARGIN - 1e-4, 1.0 - RUST_MARGIN + 1e-4, 1.0] {
                assert_eq!(
                    field(Livery::Rust, around, along),
                    0.0,
                    "the rust reaches ({around}, {along}), which is inside the margin"
                );
            }
        }
    }

    /// **The field wraps around the perimeter**, because the perimeter does.
    ///
    /// The seam is where `around` is both 0 and 1. A field that stopped at either would put
    /// a bare stripe down the one edge of the blade that has no edge.
    #[test]
    fn the_rust_field_is_continuous_across_the_seam() {
        for step in 0..=32 {
            let along = 0.5 + (step as f32 / 32.0 - 0.5) * (1.0 - 2.0 * RUST_MARGIN);
            assert!(
                (field(Livery::Rust, 0.0, along) - field(Livery::Rust, 1.0, along)).abs() < 1e-6,
                "the field disagrees with itself across the seam at {along}"
            );
        }
    }

    /// **Every un-liveried vertex lands on white**, which is the property the whole
    /// one-material arrangement rests on.
    #[test]
    fn the_neutral_uv_lands_in_the_neutral_band() {
        let [_, v] = neutral_uv();
        let row = (v * LIVERY_HEIGHT as f32).floor() as u32;
        assert!(
            row < NEUTRAL_ROWS,
            "the neutral texture coordinate samples row {row}, which is rust rather than \
             identity"
        );
        assert_eq!(
            texel(&image_for(Livery::Rust), 32, row),
            [255, 255, 255, 255]
        );
    }

    /// **A blade's coordinates never reach the neutral band** — the same property from the
    /// other side: a freckle at the root must not be squeezed onto the white row.
    #[test]
    fn a_blades_coordinates_stay_out_of_the_neutral_band() {
        let floor = NEUTRAL_ROWS as f32 / LIVERY_HEIGHT as f32;
        for step in 0..=64 {
            let along = step as f32 / 64.0;
            let [_, v] = blade_uv(0.5, along);
            // The far end is `v == 1.0` exactly, which is the image's edge rather than a
            // row: `ImageAddressMode::ClampToEdge` resolves it to the last one. What must
            // never happen is the near end reaching *up* into the band.
            assert!(
                (floor..=1.0).contains(&v),
                "a blade at {along} samples {v}, outside the rows the field was written into"
            );
        }
        assert_eq!(
            blade_uv(0.5, 0.0)[1],
            floor,
            "the root has left the field's first row"
        );
    }
}
