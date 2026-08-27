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

/// How many texels one livery's band carries along a blade.
///
/// **Sixty-three, and the number is inherited rather than chosen**: it is what the single
/// rust band occupied when the image was 64 rows with one neutral row above it. Keeping it
/// means every texel of the rust is exactly where it was, which is what lets
/// [`the_rust_livery_is_generated_the_same_way_every_time`] keep pinning the same values
/// across a change that doubled the image.
const FIELD_ROWS: u32 = 63;

/// How many texels the image carries along a blade: the neutral band, then one field band
/// per livery.
///
/// **One image for every livery, and the count of images is the count of materials the
/// renderer has to bind.** A second image would be a second binding for a mechanism whose
/// whole point is that four surfaces share one handle; it would need its reasoning written
/// down, and there is none. The height is not a power of two and does not need to be —
/// there are no mipmaps here and the sampler is nearest.
const LIVERY_HEIGHT: u32 = NEUTRAL_ROWS + FIELD_ROWS * Livery::ALL.len() as u32;

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

/// How deep the flat over the ridge is, as a fraction of the base colour.
///
/// The centre of a blade was never ground the way its bevels were. Weighted toward the flat
/// and away from the cutting edge, so the bevel keeps more of the base.
const FORGE_FLAT: f32 = 0.115;

/// How deep the hammer banding is.
const FORGE_BANDING: f32 = 0.07;

/// How deep the grinding streaks are.
const FORGE_GRINDING: f32 = 0.045;

/// How deep the forge scale is where it is present.
///
/// **The deepest term, and the one the other three are measured against.** It is also the
/// only one that reads at a glance, which is why it is a sparse scatter rather than
/// something continuous.
const FORGE_SCALE: f32 = 0.30;

/// How many hammer bands run across the blade.
const FORGE_BAND_COUNT: f32 = 7.0;

/// How many grinding streaks run around the perimeter.
///
/// Fine across the blade and continuous along it, which is the direction a stone is drawn.
const FORGE_STREAK_COUNT: f32 = 9.0;

/// How many flecks of forge scale the field carries.
const FORGE_FLECKS: u32 = 9;

/// The radius of one fleck, in the field's own `(around, along)` space.
///
/// Smaller than a rust freckle: scale is what did not come off, not what grew.
const FORGE_FLECK_RADIUS: f32 = 0.055;

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

/// How much darker one livery is than the metal it dresses, at full strength.
///
/// **A multiplier, not a colour**, and that is what keeps `player/items.rs` the one answer
/// to which colour an item presents as. The image is white — identity — everywhere the
/// field is zero, so the base that comes through is whatever that table says.
///
/// **The two directions are opposite on purpose.** Oxide goes warm: red kept, green and blue
/// pulled down, which turns a pale iron into rust rather than into grey. Forged steel goes
/// blue-grey where it darkens, because `ForgedSteel` **is** the polished value and a livery
/// can only take something away — so what it takes away is the polish. That is what lets a
/// player tell the two blades apart at a distance without looking at any detail, and it
/// costs one row rather than a second mechanism.
fn tint(livery: Livery) -> [f32; 3] {
    match livery {
        Livery::WornSteel => [0.72, 0.38, 0.22],
        // At full strength this is about 71% of the base and cooler than it in every
        // channel. The interactive model this was tuned in bottomed out near 69%; the third
        // decimal of that is not something a screen was going to settle.
        Livery::ForgedSteel => [0.64, 0.70, 0.80],
    }
}

/// How deep one livery eats into the metal, as a fraction of the local half-thickness.
///
/// **Zero is a real answer and it is what makes the second livery a row.** Corrosion eats
/// metal, so worn steel displaces; forge marks are the record of work done to a surface that
/// is still whole, so forged steel does not. A blade whose livery does not displace is
/// lofted exactly as an un-liveried one is — same sections, same vertices — and the whole of
/// what it wears is colour.
pub(super) fn pit_depth(livery: Livery) -> f32 {
    match livery {
        Livery::WornSteel => 0.30,
        Livery::ForgedSteel => 0.0,
    }
}

/// How strong one livery is at a point on the surface it dresses, in `0.0..=1.0`.
///
/// `around` runs the perimeter and **wraps**; `along` runs from the guard to the tip and
/// does not. Both are read by the pixels below and by the blade's own vertices, which is
/// what makes the pitting and the oxide the same fact rather than two that agree.
pub(super) fn field(livery: Livery, around: f32, along: f32) -> f32 {
    match livery {
        Livery::WornSteel => rust_field(around, along),
        Livery::ForgedSteel => forge_field(around, along),
    }
}

/// How far across a flat a point is, in `0.0..=1.0`, and `0.0` on the cutting edges.
///
/// The blade's section is a hexagon: `around` 0 and 1/2 are the two edges, and the two flats
/// span the sixths either side of 1/4 and 3/4. **The first two forge terms are weighted by
/// this and away from the edge**, which is what makes the bevel keep more of the base colour
/// and read as an edge without anything being brightened.
fn over_the_flat(around: f32) -> f32 {
    // Fold the perimeter onto one half: the two faces of a blade are the same face.
    let half = (around % 0.5) * 2.0;
    // 0 at the edge, 1 at the middle of the flat.
    (1.0 - (half - 0.5).abs() * 2.0).clamp(0.0, 1.0)
}

/// The forge field: four terms, at the depths an interactive model was tuned to.
///
/// They **add** rather than compete, because they are four different things that happened to
/// one piece of steel. The depths are relative to the deepest of them, so the ratios the
/// model settled survive being expressed as one strength: flat over the ridge −11.5%, hammer
/// banding −7%, grinding streaks −4.5%, forge scale −30% where present.
fn forge_field(around: f32, along: f32) -> f32 {
    let flat = over_the_flat(around);

    // The centre was never ground the way the bevels were.
    let unground = FORGE_FLAT / FORGE_SCALE * flat;
    // Forge work, across the blade.
    let banding = FORGE_BANDING / FORGE_SCALE
        * flat
        * (0.5 + 0.5 * (along * FORGE_BAND_COUNT * std::f32::consts::TAU).sin());
    // Grinding, along the blade and fine across it.
    let grinding = FORGE_GRINDING / FORGE_SCALE
        * (0.5 + 0.5 * (around * FORGE_STREAK_COUNT * std::f32::consts::TAU).sin());
    // A sparse scatter, and the only term that reads at a glance.
    let mut scale = 0.0_f32;
    for fleck in 0..FORGE_FLECKS {
        let centre_along = (fleck as f32 + scatter(fleck, 4)) / FORGE_FLECKS as f32;
        let centre_around = scatter(fleck, 5);
        let radius = FORGE_FLECK_RADIUS * (0.4 + 0.6 * scatter(fleck, 3));
        let round = (around - centre_around).abs();
        let across = round.min(1.0 - round);
        let gap = along - centre_along;
        let distance = (across * across + gap * gap).sqrt() / radius;
        scale = scale.max((1.0 - distance * distance).max(0.0));
    }

    (unground + banding + grinding + scale).clamp(0.0, 1.0)
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

/// The region of the image one livery's field occupies, in texels.
///
/// **For `bevy_ui`, which samples a rectangle rather than a coordinate.** A mesh points its
/// vertices at [`blade_uv`] and never sees the neutral band; an `ImageNode` draws the whole
/// image unless told otherwise, so the cell would put the white row across the top of every
/// blade it draws. This is that row taken off.
pub(crate) fn field_rect(livery: Livery) -> Rect {
    let top = band_top(livery);
    Rect::new(0.0, top, LIVERY_WIDTH as f32, top + FIELD_ROWS as f32)
}

/// The texture coordinate every vertex that wears no livery carries.
///
/// The centre of a texel in the neutral band, so the nearest-neighbour sampler lands on
/// white exactly and no arithmetic anywhere else has to know the band exists.
pub(super) fn neutral_uv() -> [f32; 2] {
    [0.5, 0.5 / LIVERY_HEIGHT as f32]
}

/// Whether one texture coordinate falls inside one livery's own band.
///
/// **What "one image for every livery" is worth is that this is checkable.** With a band per
/// material, a mesh reading the wrong rows is a mesh wearing another metal's surface, and
/// the sweeps in `hands.rs` measure every item against it.
///
/// Test-only: a renderer never asks, because the coordinates it draws with came from
/// [`blade_uv`] and are inside the band by construction. This is the *other* side of that
/// construction, which is exactly what a test should be reading.
#[cfg(test)]
pub(super) fn band_holds(livery: Livery, uv: [f32; 2]) -> bool {
    let row = (uv[1] * LIVERY_HEIGHT as f32).floor();
    let top = band_top(livery);
    (top..top + FIELD_ROWS as f32).contains(&row)
}

/// The first row of one livery's band.
fn band_top(livery: Livery) -> f32 {
    (NEUTRAL_ROWS + FIELD_ROWS * livery.band() as u32) as f32
}

/// The texture coordinate for a point on a blade wearing one livery.
///
/// `along` is squeezed into that livery's own band rather than spanning the image, which is
/// what lets one image serve every material: a mesh only ever samples the rows its own
/// material was written into.
pub(super) fn blade_uv(livery: Livery, around: f32, along: f32) -> [f32; 2] {
    // **Texel centres, first to last**, rather than the band's two edges. Spanning the edges
    // put `along == 1.0` — the tip — exactly on the first row of the *next* material's band,
    // which is the one place a shared image can go wrong and is why every coordinate is now
    // squarely inside a texel rather than relying on the sampler to clamp.
    [
        around,
        (band_top(livery) + 0.5 + along * (FIELD_ROWS - 1) as f32) / LIVERY_HEIGHT as f32,
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
    let along = (v * LIVERY_HEIGHT as f32 - band_top(livery) - 0.5) / (FIELD_ROWS - 1) as f32;
    if !(0.0..=1.0).contains(&along) {
        // A coordinate outside this livery's own band reaches none of its field, which is
        // the honest answer rather than an extrapolation.
        return 0.0;
    }
    field(livery, around, along)
}

/// Every livery, as the one image every material samples.
fn livery_image() -> Image {
    let mut data = Vec::with_capacity((LIVERY_WIDTH * LIVERY_HEIGHT * 4) as usize);
    for row in 0..LIVERY_HEIGHT {
        for column in 0..LIVERY_WIDTH {
            let around = (column as f32 + 0.5) / LIVERY_WIDTH as f32;
            let band = row.checked_sub(NEUTRAL_ROWS).map(|offset| {
                let livery = Livery::ALL[(offset / FIELD_ROWS) as usize];
                let along = ((offset % FIELD_ROWS) as f32 + 0.5) / FIELD_ROWS as f32;
                (livery, field(livery, around, along))
            });
            let (colour, strength) = match band {
                Some((livery, strength)) => (tint(livery), strength),
                None => ([1.0; 3], 0.0),
            };
            for channel in colour {
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
pub(crate) struct Liveries {
    image: Handle<Image>,
}

impl Liveries {
    /// The image any material that may draw a liveried item must carry.
    ///
    /// **One image, whatever the material**, which is what keeps a livery a row rather than
    /// a binding: the neutral band and every livery's band are in it, so one material serves
    /// a rusty blade, a forged one and a bare fist in a single draw. There is no per-livery
    /// choice to make here, and a second image would need its reasoning written down.
    pub(crate) fn material_image(&self) -> Handle<Image> {
        self.image.clone()
    }
}

impl FromWorld for Liveries {
    fn from_world(world: &mut World) -> Self {
        let mut images = world.resource_mut::<Assets<Image>>();
        Self {
            image: images.add(livery_image()),
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
/// one plugin is added twice, and the surfaces that draw a liveried item are four modules
/// rather than one, none of which can know whether another has already asked.
/// `App::init_resource` is idempotent, so every caller may say what it needs.
///
/// **`App::init_asset` is not, and the guard below is not defensive programming.** It ends
/// `self.insert_resource(assets)` on a freshly defaulted `Assets<A>`, unconditionally — so
/// calling it after `ImagePlugin` **replaces the whole image store**, dropping every image
/// the renderer had already put there. That includes `FallbackImage`, whose D3 entry is what
/// the mesh view bind group binds when there is no irradiance volume, and the client died on
/// startup with `Texture binding 18 expects dimension = D3, but given a view with dimension
/// = D2`.
///
/// **Nothing in this repository's test suite could have seen it**, which is the part worth
/// recording. Every test here is headless: there is no render app, no `FallbackImage`, and no
/// bind group to validate — and each one *builds* `Assets<Image>` itself, so the reset lands
/// on an empty store and changes nothing. The gates were green, the review was clean, and it
/// was found by running the game. [`registering_twice_keeps_the_images_already_loaded`] is
/// the headless half that can be pinned: not the bind group, but the reset that caused it.
pub(super) fn register(app: &mut App) {
    if !app.world().contains_resource::<Assets<Image>>() {
        // For the focused headless tests, which build one module without `ImagePlugin` — the
        // same defence `HandsPlugin` keeps for the four resources it does not own.
        app.init_asset::<Image>();
    }
    app.init_resource::<Liveries>();
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;

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

    /// **Registering twice keeps the images already loaded**, which is the property the
    /// client's startup crash was the absence of.
    ///
    /// `App::init_asset` ends in an unconditional `insert_resource` of a defaulted store, so
    /// a second call after `ImagePlugin` throws away every image the renderer put there —
    /// `FallbackImage` among them, and its D3 entry is what the mesh view bind group binds
    /// when there is no irradiance volume. The observable failure was a validation error at
    /// startup and a window that closed itself; what is checkable without a GPU is the reset
    /// underneath it.
    ///
    /// **Two readings, and the foreign image is the one that stands for what broke.**
    /// `App::init_resource` *is* idempotent, so a second `register` does not re-run
    /// `Liveries::from_world`: the handle it already holds is left pointing into the store
    /// that was thrown away, and asserting it still resolves catches the reset too. Both
    /// clauses below therefore fail against the bug — verified by removing the guard and
    /// running each on its own.
    ///
    /// The foreign image is kept first because it is the closer analogue of the failure: what
    /// actually took the client down was not this module's image going missing but
    /// *somebody else's* — `FallbackImage`, put in the store by the renderer, which this
    /// module has no handle to and no test here can name. An image added from outside is the
    /// nearest thing a headless test has to one.
    #[test]
    fn registering_twice_keeps_the_images_already_loaded() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        register(&mut app);

        // Stand in for a fallback: an image this module did not create and does not know
        // about, put in the store before the second registration.
        let foreign = app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(livery_image());

        register(&mut app);

        assert!(
            app.world().resource::<Assets<Image>>().contains(&foreign),
            "registering a second time emptied the image store, so every texture the \
             renderer had already loaded went with it"
        );
        // And the livery's own handle still resolves, which is the other half: a guard that
        // skipped too much would leave `Liveries` pointing at nothing.
        let handle = app.world().resource::<Liveries>().material_image();
        assert!(
            app.world().resource::<Assets<Image>>().contains(&handle),
            "the livery handle no longer names an image in the store"
        );
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
        let image = livery_image();

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
        // The deepest texel the field reaches, which is all but exactly worn steel's own tint —
        // the freckle's own centre falls between texel centres, so it is one part in 255
        // short of it rather than equal to it. That is worth pinning as the number it is:
        // a livery that started clamping, or one whose tint drifted, changes it.
        assert_eq!(texel(&image, 58, 10), [184, 98, 58, 255]);
        // And one in the shoulder of a freckle, so the falloff is pinned as well as the
        // peak: a field that became a hard-edged disc would keep the value above and lose
        // this one.
        assert_eq!(texel(&image, 40, 30), [207, 148, 121, 255]);

        // And each band's whole buffer, so a change that misses every named texel still has
        // to be looked at rather than merely passing. **Counted per band**, because the two
        // materials share an image and one total would let either drift under the other's
        // cover.
        // Rust is sparse, so what pins it is how much of its band it covers. Forged steel is
        // a continuous surface treatment and covers all of its own, so what pins *it* is the
        // two numbers the interactive model was tuned to: how dark it is on average, and how
        // dark it gets at its deepest.
        let band_texels = |livery: Livery| -> Vec<[u8; 4]> {
            let top = band_top(livery) as u32;
            (top..top + FIELD_ROWS)
                .flat_map(|row| (0..LIVERY_WIDTH).map(move |column| (column, row)))
                .map(|(column, row)| texel(&image, column, row))
                .collect()
        };

        let rusted = band_texels(Livery::WornSteel)
            .into_iter()
            .filter(|texel| *texel != [255, 255, 255, 255])
            .count();
        assert_eq!(
            rusted,
            709,
            "the rust covers {rusted} texels of {}, which is not what it covered",
            LIVERY_WIDTH * FIELD_ROWS
        );

        let forged = band_texels(Livery::ForgedSteel);
        let brightness =
            |texel: &[u8; 4]| f32::from(texel[0]) + f32::from(texel[1]) + f32::from(texel[2]);
        let mean = forged.iter().map(brightness).sum::<f32>() / forged.len() as f32 / (3.0 * 255.0);
        let darkest = forged.iter().map(brightness).fold(f32::INFINITY, f32::min) / (3.0 * 255.0);
        // **The two numbers the interactive model this was designed against reported**: it
        // averaged about 91% of the base colour and bottomed out near 69%. These are what
        // the four terms at their tabulated depths actually produce, to three decimals, and
        // they are pinned rather than described because re-tuning any one term moves them.
        assert!(
            (mean - 0.901).abs() < 5e-3,
            "forged steel averages {mean} of the base colour, not the 0.901 it was tuned to"
        );
        assert!(
            (darkest - 0.714).abs() < 5e-3,
            "forged steel bottoms out at {darkest}, not the 0.714 it was tuned to"
        );

        // **And it darkens toward blue, where the rust darkens toward red.** That is what
        // lets a player tell the two blades apart at a distance without looking at any
        // detail, and it is one row rather than a second mechanism — so it is asserted as a
        // direction rather than as a colour.
        let coolest = forged
            .iter()
            .min_by_key(|texel| texel[0])
            .expect("the band has texels");
        assert!(
            coolest[2] > coolest[0],
            "forged steel's deepest texel is {coolest:?}, which is not cooler than its base"
        );
        let warmest = band_texels(Livery::WornSteel)
            .into_iter()
            .min_by_key(|texel| texel[2])
            .expect("the band has texels");
        assert!(
            warmest[0] > warmest[2],
            "rust's deepest texel is {warmest:?}, which is not warmer than its base"
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
                    field(Livery::WornSteel, around, along),
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
                (field(Livery::WornSteel, 0.0, along) - field(Livery::WornSteel, 1.0, along)).abs()
                    < 1e-6,
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
        assert_eq!(texel(&livery_image(), 32, row), [255, 255, 255, 255]);
    }

    /// **The cell samples exactly the rows a mesh does**, which is the one place the two
    /// consumers of this image could disagree.
    ///
    /// A mesh points its vertices at [`blade_uv`] and a cell hands `bevy_ui` a rectangle, so
    /// they arrive at the field by different arithmetic: one squeezes `along` past the
    /// neutral band, the other names the band's rows. Asserted against the *image* rather
    /// than against either of them, so a rectangle that cropped the field or overran the
    /// image fails here rather than in somebody's eye.
    ///
    /// **[`LIVERY_HEIGHT`] is the whole image, the neutral band included** — its own doc says
    /// so — which is what makes `y_max` the image's height rather than the field's. This
    /// exists because that is easy to read the other way round, and the review on the pull
    /// request that added `field_rect` read it that way.
    #[test]
    fn the_cell_and_the_mesh_sample_the_same_rows() {
        let image = livery_image();
        let white = |row: u32| {
            (0..LIVERY_WIDTH).all(|column| texel(&image, column, row) == [255, 255, 255, 255])
        };

        for livery in Livery::ALL {
            let rect = field_rect(livery);
            assert_eq!(rect.min.x, 0.0);
            assert_eq!(rect.max.x, LIVERY_WIDTH as f32);
            assert!(
                rect.max.y <= LIVERY_HEIGHT as f32,
                "{livery:?}'s rectangle runs off the bottom of a {LIVERY_HEIGHT}-row image"
            );
            assert!(
                rect.min.y >= NEUTRAL_ROWS as f32,
                "{livery:?}'s rectangle reaches up into the neutral band"
            );

            // The rows a **mesh** reaches, read off `blade_uv` at both ends, are the first
            // and last rows the rectangle covers.
            let row = |along: f32| (blade_uv(livery, 0.5, along)[1] * LIVERY_HEIGHT as f32).floor();
            assert_eq!(
                row(0.0),
                rect.min.y,
                "{livery:?}: a blade's root samples a row its rectangle does not cover"
            );
            assert_eq!(
                row(1.0),
                rect.max.y - 1.0,
                "{livery:?}: a blade's tip samples a row its rectangle does not cover"
            );

            // And no other livery's band is inside it, which is the failure one shared image
            // makes possible and a single-band image could not.
            for other in Livery::ALL {
                if other == livery {
                    continue;
                }
                let theirs = field_rect(other);
                assert!(
                    theirs.max.y <= rect.min.y || theirs.min.y >= rect.max.y,
                    "{livery:?}'s rectangle overlaps {other:?}'s"
                );
            }
        }

        // **Every row no rectangle covers is white**, which is the neutral band and nothing
        // else. Not "every row inside a rectangle carries marks" — a field deliberately
        // leaves both ends of the blade clear, so rows near the guard and the point are white
        // and belong in the rectangle anyway.
        let covered = |row: u32| {
            Livery::ALL.iter().any(|livery| {
                let rect = field_rect(*livery);
                (rect.min.y as u32..rect.max.y as u32).contains(&row)
            })
        };
        for row in 0..LIVERY_HEIGHT {
            if !covered(row) {
                assert!(
                    white(row),
                    "row {row} is covered by no rectangle and is not neutral"
                );
            }
        }
        for livery in Livery::ALL {
            let rect = field_rect(livery);
            assert!(
                (rect.min.y as u32..rect.max.y as u32).any(|row| !white(row)),
                "{livery:?}'s rectangle covers no field at all"
            );
        }
    }

    /// **A blade's coordinates never reach the neutral band** — the same property from the
    /// other side: a freckle at the root must not be squeezed onto the white row.
    #[test]
    fn a_blades_coordinates_stay_out_of_the_neutral_band() {
        // **Every livery, and both ends of every band.** With one band per material the
        // failure this guards is no longer only "a blade reaching up into the white row" —
        // it is a blade reaching into the *next material's* rows, which is what
        // `along == 1.0` did before the mapping moved to texel centres: the tip landed
        // exactly on the first row forged steel was written into.
        for livery in Livery::ALL {
            for step in 0..=64 {
                let along = step as f32 / 64.0;
                assert!(
                    band_holds(livery, blade_uv(livery, 0.5, along)),
                    "a blade at {along} leaves {livery:?}'s own band"
                );
            }
            // The two ends land on the band's first and last rows rather than short of them,
            // so a freckle at the root or the tip is drawn rather than squeezed out.
            let row = |along: f32| {
                (blade_uv(livery, 0.5, along)[1] * LIVERY_HEIGHT as f32).floor() as u32
            };
            assert_eq!(row(0.0), band_top(livery) as u32);
            assert_eq!(row(1.0), band_top(livery) as u32 + FIELD_ROWS - 1);
        }

        // And no band reaches the neutral row, from either side.
        assert!(
            !band_holds(Livery::ALL[0], neutral_uv()),
            "the neutral coordinate falls inside a livery's band"
        );
    }
}
