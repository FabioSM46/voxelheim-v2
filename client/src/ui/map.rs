//! The world map: the screen's viewport, the tiles it asks the server for, and the cache
//! it keeps them in.
//!
//! **Every pixel of this map is the server's.** The client keeps nothing for unloaded
//! chunks and never generates terrain, so it cannot draw a place it has not been told
//! about — `world/mod.rs` says so, and the map is the surface where that is most
//! tempting to forget. What this module owns is therefore caching, eviction and *asking*:
//! which squares the current viewport overlaps, which of those are already held, and how
//! many may be outstanding at once.
//!
//! ## Asking is bounded, and silence is an answer
//!
//! The server meters `MapTileRequest` with a token bucket, and **a spent bucket is
//! silence rather than a refusal** — there is no message that says "not now". So a request
//! that goes unanswered has to expire on this side: [`REQUEST_RETRY`] is how long a tile
//! stays in flight before the square is offered again, and [`MAX_IN_FLIGHT`] is what keeps
//! a first frame from asking for a screenful at once. Neither number decides anything; the
//! server draws the tile or it does not.
//!
//! ## The ledger evicts, it does not paint
//!
//! `MapExplored` is additive: a chunk column named once is explored for good. A page that
//! names a column inside a cached tile therefore means that tile was drawn before the
//! player had been there, so the cached copy is thrown away and asked for again. The
//! client never fills the column in itself — it has no idea what is under it.
//!
//! ## The picture is composed, never rendered
//!
//! There is no second camera and no render target here. The window is `bevy_ui` nodes and
//! the map inside it is **one `Image` the size of the viewport**, rewritten from the cache
//! whenever the view or the cache moves — a few hundred KiB of texels, which is cheaper
//! than the pipeline a render-to-texture would need and, more to the point, is a plain
//! array a headless test can read one pixel out of. [`compose`] is that rewrite, and it is
//! pure: a viewport, a cache, and the bytes that fall out.
//!
//! **Fog answers two different questions and deliberately looks like one.** A pixel whose
//! chunk column is clear in its tile's own mask is somewhere this character has not been;
//! a pixel in a square the server has not drawn yet is somewhere this client has not been
//! told about. Both are *nothing is known here*, and inventing a difference would put a
//! second kind of emptiness on the map that means nothing to the player.
//!
//! ## The pointer moves the view, and one method is how
//!
//! Panning and zooming are the same question asked twice — *put this world position back
//! under this point on the picture* — so they are one method, [`MapScreen::look_so_that`],
//! and neither accumulates anything. A drag remembers the world position it grabbed and
//! re-answers it every frame from the pointer's current place, which is why a slow drag at
//! one block per pixel does not lose fractions to an integer centre; a zoom step asks the
//! same thing of the pointer's own position, which is the whole of "the pointer's world
//! position is kept under the pointer".
//!
//! **An unnamed surface is not drawn as stone.** [`MapSurface::Unknown`] on an explored
//! pixel means this build has no name for what the server put there — a contract that has
//! grown a member since this binary was compiled. Painting it as the nearest thing would
//! be a guess the player could not tell from a measurement, so it gets a colour of its own
//! that belongs to nothing in the world.

use std::collections::HashMap;
use std::time::Duration;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::ui::{FocusPolicy, UiGlobalTransform, UiSystems};
use bevy::window::PrimaryWindow;

use super::compass::coordinates_reading;
use crate::net::{
    CHUNK_COLUMN_BLOCKS, MAP_TILE_EDGE, MapColumn, MapEvent, MapInbox, MapSurface, MapTile,
    MapTileRequest, Marker, MarkerKind, Outbound, Session, encode_map_tile_request, map_tile_span,
};
use crate::player::{InputMode, LookState, PlayerStats};

/// How far from the origin a map coordinate may be, on either axis.
///
/// The server's `world.BlockLimit`, mirrored here for the reason every mirrored bound in
/// this client exists: it is a limit on what this side may *choose*, checked where the
/// choice is made. Nothing is decided from it — a request outside the world would be
/// refused with `TileMisaligned` or answered with nothing — and what it buys locally is
/// that the viewport arithmetic below cannot overflow an `i32`.
const WORLD_EXTENT: i32 = 1 << 24;

/// The most tile requests that may be outstanding at once.
const MAX_IN_FLIGHT: usize = 8;

/// How long a request stays in flight before its square is offered again.
///
/// The server answers a request it has tokens for immediately, so this is a bound on how
/// long a *dropped* one costs — not a round-trip estimate, and not a timeout on the
/// session. Short enough that a bucket that refills at eight a second is asked again
/// promptly; long enough that one screen of tiles is not re-asked every frame.
const REQUEST_RETRY: Duration = Duration::from_secs(2);

/// The size of the map's viewport node in logical pixels, until the window measures it.
///
/// The window is what knows the real number, and it cannot know it before a layout pass.
/// This is the value the cache works from until then, so the asking is complete on its
/// own rather than waiting for something to be drawn.
const DEFAULT_VIEWPORT: UVec2 = UVec2::new(1024, 768);

/// Every look the wheel steps through, widest first.
///
/// Two knobs and one ladder. The zoom magnifies what the server already drew, so it runs
/// out at four; past that the only way closer is to ask for a finer tile, which is what
/// stepping the scale down does. **The rungs where they meet are not duplicates**: a
/// sixteen-block pixel magnified four times and a four-block pixel drawn once cover the
/// same width of world, and the second is the one made of measurements — same coverage,
/// four times the detail, which is exactly the step a player asking "what is actually
/// down there" wants.
const ZOOM_LADDER: [(MapScale, u8); 9] = [
    (MapScale::S16, 1),
    (MapScale::S16, 2),
    (MapScale::S16, 4),
    (MapScale::S4, 1),
    (MapScale::S4, 2),
    (MapScale::S4, 4),
    (MapScale::S1, 1),
    (MapScale::S1, 2),
    (MapScale::S1, 4),
];

/// The rung the map opens at: [`MapScale::S4`] at a zoom of two, in the middle of the
/// ladder, so the first turn of the wheel goes somewhere in either direction.
const DEFAULT_RUNG: usize = 4;

/// The most squares one viewport may be broken into.
///
/// A guard rather than a rule: the viewport is this client's own number, so nothing
/// hostile can drive it. What it stops is a mistake in the layout half producing an
/// allocation proportional to a garbage size — so it is checked against the grid's
/// extent *before* the squares are built, because truncating them afterwards has
/// already paid for the allocation it is guarding against and silently drops squares a
/// real viewport reaches. [`DEFAULT_VIEWPORT`] at the closest zoom is sixteen squares
/// by twelve, and a cap a screenful can touch leaves permanent holes in the map.
///
/// The number sits above every viewport and below the arithmetic's own ceiling:
/// [`MapScreen::image_size`] clamps each axis to `1 << 14` pixels, so the widest grid
/// askable is 257 by 257, while a 7680 by 4320 viewport at the closest zoom is 121 by 68.
const MAX_TILES_IN_VIEW: usize = 16_384;

/// How many blocks one map pixel covers.
///
/// A closed set and not a number, because `schemas/world.fbs` has exactly three members
/// and a scale the contract has no member for is a request the server refuses. The names
/// are the block counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum MapScale {
    /// One block per pixel: a tile is 64 blocks across.
    S1,
    /// The scale the map opens at — a tile is 256 blocks across, which is a few minutes'
    /// walk and the span a player is usually asking about.
    #[default]
    S4,
    /// Sixteen blocks per pixel: a tile is 1024 blocks across.
    S16,
}

impl MapScale {
    /// How many blocks one pixel covers.
    const fn blocks(self) -> i32 {
        match self {
            Self::S1 => 1,
            Self::S4 => 4,
            Self::S16 => 16,
        }
    }

    /// The value the wire carries. Total, which is the point of the closed set.
    const fn wire(self) -> u8 {
        match self {
            Self::S1 => 1,
            Self::S4 => 4,
            Self::S16 => 16,
        }
    }

    /// How many blocks one tile covers on each axis, and therefore the grid every tile
    /// origin sits on.
    fn span(self) -> i32 {
        // `map_tile_span` is the contract's own arithmetic and it is partial over `u8`.
        // Every member of this enum is a member of `MAP_TILE_SCALES`, so the fallback is
        // unreachable — and it is spelled rather than unwrapped, because a panic in a
        // viewport calculation is a worse answer than one square too wide.
        map_tile_span(self.wire()).unwrap_or(MAP_TILE_EDGE as i32)
    }
}

/// One square of the map, addressed the way the server addresses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct TileKey {
    origin_x: i32,
    origin_z: i32,
    scale: MapScale,
}

impl TileKey {
    /// Whether `block` lies inside this square.
    ///
    /// The composition's memo asks this of the square it drew the last pixel from, which
    /// is what keeps a row of pixels to one lookup per square rather than one per pixel.
    fn holds(self, block: IVec2) -> bool {
        let span = self.scale.span();
        (self.origin_x..self.origin_x + span).contains(&block.x)
            && (self.origin_z..self.origin_z + span).contains(&block.y)
    }

    /// Whether `column` lies inside this square.
    ///
    /// Chunk columns are the granularity the server records exploration at, so this is
    /// what turns one ledger page into a set of evictions.
    fn contains(self, column: MapColumn) -> bool {
        let span = self.scale.span();
        let x = i64::from(column.cx) * i64::from(CHUNK_COLUMN_BLOCKS);
        let z = i64::from(column.cz) * i64::from(CHUNK_COLUMN_BLOCKS);
        let inside = |value: i64, origin: i32| {
            let origin = i64::from(origin);
            value + i64::from(CHUNK_COLUMN_BLOCKS) > origin && value < origin + i64::from(span)
        };
        inside(x, self.origin_x) && inside(z, self.origin_z)
    }
}

/// The map window's viewport: where it is looking and how closely.
///
/// Presentation only. Nothing here is sent but the squares it makes this client ask for,
/// and a request is a question rather than an outcome.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MapScreen {
    open: bool,
    /// The block the middle of the viewport is on.
    centre: IVec2,
    scale: MapScale,
    /// Logical screen pixels per tile pixel: 1, 2 or 4.
    zoom: u8,
    /// The viewport node's size in logical screen pixels.
    viewport: UVec2,
}

impl Default for MapScreen {
    fn default() -> Self {
        Self {
            open: false,
            centre: IVec2::ZERO,
            scale: MapScale::default(),
            zoom: 2,
            viewport: DEFAULT_VIEWPORT,
        }
    }
}

impl MapScreen {
    /// Puts the map up, looking at `centre`.
    ///
    /// The scale and the zoom go back to their defaults with it: a map that reopened at
    /// whatever the last look happened to leave behind would answer *where am I* with a
    /// view of somewhere else.
    fn open(&mut self, centre: IVec2) {
        let clamp = |value: i32| value.clamp(-WORLD_EXTENT, WORLD_EXTENT);
        *self = Self {
            open: true,
            centre: IVec2::new(clamp(centre.x), clamp(centre.y)),
            ..Self::default()
        };
    }

    /// Takes the map down. The cache outlives it — see [`MapTiles`].
    fn close(&mut self) {
        self.open = false;
    }

    /// Whether the map window is up.
    pub(super) const fn is_open(&self) -> bool {
        self.open
    }

    /// The size of the image the map is composed into, in tile pixels.
    ///
    /// The viewport divided by the zoom: a zoom of two draws every tile pixel as a two by
    /// two square, so half as many of them fit. `max(1)` on the zoom is a guard rather
    /// than a case — the field is only ever one of three values — and it is what keeps a
    /// division out of reach of a zero.
    fn image_size(&self) -> UVec2 {
        let zoom = u32::from(self.zoom).max(1);
        UVec2::new(
            (self.viewport.x / zoom).min(1 << 14),
            (self.viewport.y / zoom).min(1 << 14),
        )
    }

    /// How many blocks the viewport spans on each axis.
    fn span_blocks(&self) -> IVec2 {
        let blocks = self.scale.blocks();
        let image = self.image_size();
        IVec2::new(image.x as i32 * blocks, image.y as i32 * blocks)
    }

    /// The size of the drawn picture in logical screen pixels.
    ///
    /// [`Self::image_size`] rounded back up by the zoom, which is not the viewport: a
    /// viewport 1023 pixels wide at a zoom of two draws 511 tile pixels and is 1022 wide
    /// once they are magnified. **The leftover pixel is the reason every projection below
    /// is written against this rather than against the node**: the picture is centred in
    /// the viewport, so a coordinate measured from the node's edge is off by half of
    /// whatever the division threw away.
    fn drawn_size(&self) -> UVec2 {
        self.image_size() * u32::from(self.zoom).max(1)
    }

    /// Whether a point on the picture is over something the picture drew.
    ///
    /// The picture is centred in a viewport it rarely divides exactly, so a pointer inside
    /// the window can still be beside the map rather than on it. A gesture does not ask
    /// this — a drag that leaves the edge is still a drag — and the readout does, because
    /// naming a block nobody is pointing at is the one answer worse than saying nothing.
    fn shows(&self, point: Vec2) -> bool {
        let drawn = self.drawn_size().as_vec2();
        (0.0..drawn.x).contains(&point.x) && (0.0..drawn.y).contains(&point.y)
    }

    /// The block the picture's top-left pixel shows.
    ///
    /// Every coordinate on the map is this plus an offset, so it is the one place the
    /// centre becomes a corner. The arithmetic cannot overflow: a centre is clamped to
    /// [`WORLD_EXTENT`] and the widest span is [`MapScale::S16`] over a picture clamped to
    /// `1 << 14` pixels, which is eighteen bits.
    fn origin_block(&self) -> IVec2 {
        let extent = self.span_blocks();
        IVec2::new(
            self.centre.x.saturating_sub(extent.x / 2),
            self.centre.y.saturating_sub(extent.y / 2),
        )
    }

    /// The world position under a point on the picture, in blocks and fractional.
    ///
    /// Fractional on purpose. A pan and a zoom both have to put a place back where it was,
    /// and rounding it to a block first is what turns a slow drag into a stuck one.
    fn world_at(&self, point: Vec2) -> Vec2 {
        let blocks = self.scale.blocks() as f32;
        let zoom = f32::from(self.zoom.max(1));
        self.origin_block().as_vec2() + point / zoom * blocks
    }

    /// Where a world position is drawn, in logical screen pixels from the picture's
    /// top-left corner.
    ///
    /// The inverse of [`Self::world_at`], and the two are a pair on purpose: the dot that
    /// says where the player is and the readout that says what the pointer is over are the
    /// same arithmetic run in opposite directions, and a map where they disagree is a map
    /// that lies about one of them.
    fn point_of(&self, world: Vec2) -> Vec2 {
        let blocks = self.scale.blocks() as f32;
        let zoom = f32::from(self.zoom.max(1));
        (world - self.origin_block().as_vec2()) / blocks * zoom
    }

    /// The block under a point on the picture.
    fn block_at(&self, point: Vec2) -> IVec2 {
        let world = self.world_at(point);
        // `floor` and not a truncating cast, for the reason `ui/compass.rs` gives, and the
        // same non-finite guard: a pointer position comes from a window rather than from
        // this module.
        IVec2::new(block_of(world.x), block_of(world.y))
    }

    /// Looks so that `world` sits under `point` on the picture.
    ///
    /// The one movement this screen has. A drag is this with the world position it grabbed
    /// and the pointer's place now; a zoom step is this with the pointer's own world
    /// position and the pointer's own place. **Nothing accumulates**, so neither gesture
    /// can drift, and a centre that has to be a whole block does not eat a slow drag one
    /// rounded fraction at a time.
    fn look_so_that(&mut self, world: Vec2, point: Vec2) {
        let blocks = self.scale.blocks() as f32;
        let zoom = f32::from(self.zoom.max(1));
        let extent = self.span_blocks().as_vec2();
        let centre = world + extent / 2.0 - point / zoom * blocks;
        self.centre = IVec2::new(block_of(centre.x), block_of(centre.y));
    }

    /// Which rung of [`ZOOM_LADDER`] this view is on, or the rung it opens at for a pair
    /// the ladder has no place for — which nothing can produce, since the ladder is the
    /// only thing that writes the pair.
    fn rung(&self) -> usize {
        ZOOM_LADDER
            .iter()
            .position(|step| *step == (self.scale, self.zoom))
            .unwrap_or(DEFAULT_RUNG)
    }

    /// Steps the view along [`ZOOM_LADDER`], keeping whatever is under `point` under it.
    ///
    /// `steps` is positive to zoom in. The ends of the ladder are ends rather than wraps:
    /// a wheel that jumped from the closest look to the widest would lose the place the
    /// player was reading.
    fn zoom_by(&mut self, steps: i32, point: Vec2) {
        let held = self.world_at(point);
        let last = ZOOM_LADDER.len() as i32 - 1;
        let rung = (self.rung() as i32 + steps).clamp(0, last) as usize;
        let (scale, zoom) = ZOOM_LADDER[rung];
        if (self.scale, self.zoom) == (scale, zoom) {
            return;
        }
        self.scale = scale;
        self.zoom = zoom;
        self.look_so_that(held, point);
    }

    /// Every square the viewport overlaps, nearest the centre first.
    ///
    /// The order is what makes [`MAX_IN_FLIGHT`] a useful bound rather than an arbitrary
    /// one: when only some of a screenful can be asked for at once, the ones under the
    /// player's eye are the ones that go out.
    fn tiles_in_view(&self) -> Vec<TileKey> {
        let span = self.scale.span();
        let extent = self.span_blocks();
        if extent.x <= 0 || extent.y <= 0 {
            return Vec::new();
        }
        // The half-open block range the viewport covers is `[min, min + extent)`, so the
        // squares it overlaps run from the one holding `min` to the one holding the last
        // block in it. `div_euclid` and not a truncating divide, because the two disagree
        // over exactly the half of the world that is negative.
        let span64 = i64::from(span);
        let bounds = |centre: i32, extent: i32| {
            let min = i64::from(centre) - i64::from(extent) / 2;
            let first = min.div_euclid(span64);
            let last = (min + i64::from(extent) - 1).div_euclid(span64);
            (first * span64, (last - first + 1) as i32)
        };

        let ((first_x, count_x), (first_z, count_z)) = (
            bounds(self.centre.x, extent.x),
            bounds(self.centre.y, extent.y),
        );
        let (first_x, first_z) = (first_x as i32, first_z as i32);
        // Counted before anything is allocated, which is the whole of what
        // `MAX_TILES_IN_VIEW` is for. A grid this wide is a layout that has handed the
        // map a size no window has, and asking for nothing is what that costs.
        let count = i64::from(count_x) * i64::from(count_z);
        if count > MAX_TILES_IN_VIEW as i64 {
            return Vec::new();
        }
        let mut tiles = Vec::with_capacity(count as usize);
        for step_z in 0..count_z {
            for step_x in 0..count_x {
                let origin_x = first_x.saturating_add(step_x.saturating_mul(span));
                let origin_z = first_z.saturating_add(step_z.saturating_mul(span));
                tiles.push(TileKey {
                    origin_x,
                    origin_z,
                    scale: self.scale,
                });
            }
        }
        // Squared distance from the viewport's centre to the square's own, in `i64`
        // because a coordinate at the world's edge squares past `i32`.
        let half = i64::from(span) / 2;
        tiles.sort_by_key(|tile| {
            let dx = i64::from(tile.origin_x) + half - i64::from(self.centre.x);
            let dz = i64::from(tile.origin_z) + half - i64::from(self.centre.y);
            (dx * dx + dz * dz, tile.origin_z, tile.origin_x)
        });
        tiles
    }
}

/// Every square the server has drawn for this session, and every one that has been asked
/// for and not yet answered.
///
/// Memory only, and per session: a tile is what the server drew *for this character*,
/// masked by that character's ledger, so it is not a thing to keep across a sign-in.
#[derive(Resource, Debug, Default)]
pub(super) struct MapTiles {
    tiles: HashMap<TileKey, MapTile>,
    /// The requests this client is still waiting for, by the square each one holds.
    in_flight: HashMap<TileKey, InFlight>,
    /// Ordering and staleness for the server, never a clock. See `MapTileRequest`.
    next_tick: u32,
    /// Bumped whenever the *drawn* squares change, and by nothing else.
    ///
    /// Bevy's own change detection cannot stand in for this: `request_map_tiles` retires
    /// expired notes through a `ResMut` every frame, so this resource is "changed" on
    /// every frame the map is open whether or not a single pixel moved. Composing the
    /// picture from that would rewrite a few hundred KiB sixty times a second to draw
    /// exactly what was already on the screen.
    revision: u64,
}

/// One request that has gone out and not been answered.
#[derive(Debug, Clone, Copy)]
struct InFlight {
    /// When this note stops holding its square, on `Time`'s clock.
    ///
    /// A deadline rather than a departure time, so the expiry is one comparison and a
    /// test can retire a note by writing a number instead of moving a clock.
    expires: Duration,
    /// A chunk column that became explored while this request was outstanding, if one
    /// did. See [`MapTiles::evict`] for what it is for.
    overtaken: Option<MapColumn>,
}

impl MapTiles {
    /// Records one drawn square. A tile the client never asked for is kept all the same —
    /// the server is the authority on what it sends, and a square is a square.
    ///
    /// The one answer thrown away is one the ledger has overtaken, and the tile settles
    /// that itself: a tile that does not know about the column that evicted its square
    /// was drawn before that column was explored. Dropping the note with it offers the
    /// square again on the next pass.
    fn insert(&mut self, tile: MapTile) {
        let Some(key) = key_of(&tile) else {
            return;
        };
        let overtaken = self.in_flight.remove(&key).and_then(|note| note.overtaken);
        if overtaken.is_some_and(|column| drawn_with(&tile, column) == Some(false)) {
            return;
        }
        self.tiles.insert(key, tile);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Throws away every square `column` falls inside, so each is asked for again.
    ///
    /// A square with a request still outstanding has nothing to throw away yet, and its
    /// note is marked rather than dropped. **The answer in the mail may or may not be
    /// stale, and only it knows which**: `DrawMapTile` runs on the session's read loop
    /// while the ledger's pages leave on the streaming goroutine, so a tile drawn before
    /// this column was explored can still be enqueued after the page that names it.
    /// Dropping the note would ask a second time for a square whose first answer is
    /// still coming, and would cache that answer whichever it was.
    fn evict(&mut self, column: MapColumn) {
        let held = self.tiles.len();
        self.tiles.retain(|key, _| !key.contains(column));
        if self.tiles.len() != held {
            self.revision = self.revision.wrapping_add(1);
        }
        for (key, note) in &mut self.in_flight {
            if key.contains(column) {
                note.overtaken = Some(column);
            }
        }
    }

    /// Forgets everything. The end of a session, and nothing else.
    fn clear(&mut self) {
        self.tiles.clear();
        self.in_flight.clear();
        self.revision = self.revision.wrapping_add(1);
    }

    /// The square holding `block` at `scale`, if this client is holding it.
    fn holding(&self, block: IVec2, scale: MapScale) -> Option<(TileKey, &MapTile)> {
        let span = scale.span();
        let key = TileKey {
            origin_x: block.x.div_euclid(span) * span,
            origin_z: block.y.div_euclid(span) * span,
            scale,
        };
        self.tiles.get(&key).map(|tile| (key, tile))
    }
}

/// Whether `tile` was drawn with `column` already explored, or `None` for a column that
/// is not one of the tile's own — which [`TileKey::contains`] has already settled, so it
/// is a guard rather than a case.
///
/// The server's `drawMapTile` fills the `explored` mask from the same ledger it then
/// draws the pixels from, one bit per chunk column, row-major with z outer and LSB first
/// within each byte. The mask is therefore not a hint about the tile — it *is* the ledger
/// the tile was drawn from, which is what lets a client holding a newer page tell an
/// overtaken answer from a fresh one without a clock.
fn drawn_with(tile: &MapTile, column: MapColumn) -> Option<bool> {
    let span = i64::from(map_tile_span(tile.scale)?);
    let columns = i64::from(CHUNK_COLUMN_BLOCKS);
    let edge = span / columns;
    // `div_euclid` for the reason every other coordinate here uses it: a tile origin is
    // a multiple of its span and every span is a multiple of a chunk column, so this is
    // exact on both sides of the origin where a truncating divide is not.
    let cx = i64::from(column.cx) - i64::from(tile.origin_x).div_euclid(columns);
    let cz = i64::from(column.cz) - i64::from(tile.origin_z).div_euclid(columns);
    if cx < 0 || cz < 0 || cx >= edge || cz >= edge {
        return None;
    }
    let bit = (cz * edge + cx) as usize;
    let byte = tile.explored.get(bit / 8)?;
    Some(byte & (1u8 << (bit % 8)) != 0)
}

/// The square a drawn tile belongs to, or `None` for a scale this client has no member
/// for — which the codec has already refused, so it is a guard rather than a case.
fn key_of(tile: &MapTile) -> Option<TileKey> {
    let scale = match tile.scale {
        1 => MapScale::S1,
        4 => MapScale::S4,
        16 => MapScale::S16,
        _ => return None,
    };
    Some(TileKey {
        origin_x: tile.origin_x,
        origin_z: tile.origin_z,
        scale,
    })
}

/// What a pixel nobody knows anything about reads as.
///
/// Dark enough that the drawn world is unmistakably the lit part of the picture, and not
/// black, so the map still reads as a sheet with something on it rather than as a hole in
/// the window.
const FOG: [f32; 3] = [0.03, 0.035, 0.05];

/// The colour every surface is drawn from, before the ground's height shades it.
///
/// **Linear, not sRGB, and the texture format is what decides that.** These triples go
/// into an `Rgba8Unorm` image, whose texels the renderer reads as linear values — the same
/// reason `player/livery.rs` gives for its own generated image. Spelling them as
/// `Color::srgb` numbers here would have them come out lighter than they read.
///
/// Total over [`MapSurface`] with no wildcard arm, so a surface the contract grows is a
/// build failure in this function rather than a colour somebody has to notice is wrong.
const fn surface_tint(surface: MapSurface) -> [f32; 3] {
    match surface {
        // Nothing in the world is this colour, which is the point: it says *this build
        // cannot name what is here*, and it must not be mistakable for a measurement.
        MapSurface::Unknown => [0.42, 0.10, 0.40],
        MapSurface::Grass => [0.13, 0.30, 0.11],
        MapSurface::Snow => [0.88, 0.90, 0.94],
        MapSurface::Sand => [0.66, 0.55, 0.28],
        MapSurface::Stone => [0.34, 0.35, 0.37],
        MapSurface::Gravel => [0.33, 0.29, 0.24],
        MapSurface::Water => [0.04, 0.09, 0.34],
        MapSurface::Ice => [0.66, 0.78, 0.88],
        MapSurface::Forest => [0.06, 0.17, 0.07],
        MapSurface::Cave => [0.03, 0.03, 0.04],
        MapSurface::Settlement => [0.72, 0.36, 0.10],
    }
}

/// What the lowest ground multiplies its colour by, and what the highest does.
///
/// One rule for every surface rather than a table with an exception per member: relief is
/// what turns a green rectangle into a coastline, and a shading that some surfaces obeyed
/// and others did not would read as an error in the map rather than as a decision. The
/// span is deliberately narrow — a map is read for *where*, and a slope dark enough to be
/// mistaken for another surface would be answering a different question.
const SHADE_LOW: f32 = 0.62;
const SHADE_HIGH: f32 = 1.28;

/// One explored pixel, as bytes.
///
/// `height` is the server's biased byte — `clamp(y + 64, 0, 255)` — and it is used as a
/// shade and never as a coordinate, which is what lets this be a multiply with no idea
/// where sea level is.
fn shaded(surface: MapSurface, height: u8) -> [u8; 3] {
    let lift = SHADE_LOW + (SHADE_HIGH - SHADE_LOW) * (f32::from(height) / 255.0);
    surface_tint(surface).map(|channel| ((channel * lift).clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The colour one block reads as inside the square that holds it, or `None` where that
/// square says the character's ledger had not reached it.
///
/// `None` is fog, and it is the same `None` a square nobody holds produces — see the
/// module comment on why the two are one colour.
fn painted(tile: &MapTile, block: IVec2) -> Option<[u8; 3]> {
    let blocks = i32::from(tile.scale).max(1);
    let cell_x = (block.x - tile.origin_x).div_euclid(blocks);
    let cell_z = (block.y - tile.origin_z).div_euclid(blocks);
    let edge = MAP_TILE_EDGE as i32;
    if !(0..edge).contains(&cell_x) || !(0..edge).contains(&cell_z) {
        return None;
    }
    // The mask the tile was drawn from, read through the same helper the eviction rule
    // uses. A pixel is explored exactly when its chunk column is, because that is the
    // granularity the server's ledger has.
    let column = MapColumn {
        cx: block.x.div_euclid(CHUNK_COLUMN_BLOCKS),
        cz: block.y.div_euclid(CHUNK_COLUMN_BLOCKS),
    };
    if drawn_with(tile, column) != Some(true) {
        return None;
    }
    let index = (cell_z * edge + cell_x) as usize;
    Some(shaded(*tile.surface.get(index)?, *tile.height.get(index)?))
}

/// Draws the viewport into a fresh image, reading only squares this client is holding.
///
/// Pure, and the whole of what "the map is composed" means: every pixel is one lookup into
/// the cache and one colour, with no state of its own to fall out of step. The one piece
/// of cleverness is the memo — a row of pixels walks left to right through a square 64
/// wide, so the lookup it needs is nearly always the one it just did.
fn compose(screen: &MapScreen, tiles: &MapTiles) -> Image {
    let size = screen.image_size();
    let origin = screen.origin_block();
    let blocks = screen.scale.blocks();
    let fog = FOG.map(|channel| (channel * 255.0).round() as u8);

    let mut data = Vec::with_capacity((size.x as usize * size.y as usize) * 4);
    let mut held: Option<(TileKey, &MapTile)> = None;
    for row in 0..size.y {
        let block_z = origin.y.saturating_add(row as i32 * blocks);
        for column in 0..size.x {
            let block = IVec2::new(origin.x.saturating_add(column as i32 * blocks), block_z);
            if !held.is_some_and(|(key, _)| key.holds(block)) {
                held = tiles.holding(block, screen.scale);
            }
            let colour = held
                .and_then(|(_, tile)| painted(tile, block))
                .unwrap_or(fog);
            data.extend_from_slice(&colour);
            data.push(u8::MAX);
        }
    }

    // A viewport with no pixels in it is a layout that has not run yet rather than a
    // picture of nothing. `Image` requires the extent and the data to agree, so one fog
    // texel stands in; the node it is drawn into is zero-sized anyway.
    let (extent, data) = if data.is_empty() {
        (UVec2::ONE, vec![fog[0], fog[1], fog[2], u8::MAX])
    } else {
        (size, data)
    };

    let mut image = Image::new(
        Extent3d {
            width: extent.x,
            height: extent.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        // Both worlds, so a headless test can read the texels back out of the main-world
        // store — the same reason `player/livery.rs` keeps its field readable.
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        // **Nearest, and not a taste.** A map pixel is a measurement of a square of world,
        // and a zoom of four is four screen pixels showing that one measurement. Blending
        // them would draw a coastline the server never sent.
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        ..default()
    });
    image
}

/// The picture every map viewport draws, as one asset rewritten in place.
///
/// One handle for the life of the app rather than one per composition: the `ImageNode` is
/// spawned once and never learns about a new handle, and an asset store that grew an entry
/// every time the player panned would be a leak with a scroll wheel on it.
#[derive(Resource, Debug, Clone)]
struct MapPicture(Handle<Image>);

impl FromWorld for MapPicture {
    fn from_world(world: &mut World) -> Self {
        let mut images = world.resource_mut::<Assets<Image>>();
        // A viewport of nothing, so the app starts with one texel rather than the third
        // of a megabyte a default-sized picture of fog would be. The first frame the map
        // is open composes the real thing.
        let empty = MapScreen {
            viewport: UVec2::ZERO,
            ..MapScreen::default()
        };
        Self(images.add(compose(&empty, &MapTiles::default())))
    }
}

/// What the last composition was of, so the next frame can tell whether there is anything
/// new to draw.
///
/// The view and the cache's revision together: panning changes the first, a tile arriving
/// changes the second, and nothing else is a reason to rewrite a few hundred KiB.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Painted(Option<(MapScreen, u64)>);

/// Where the pointer is on the picture, in logical pixels from its top-left corner, or
/// `None` when the window has no pointer.
///
/// **Deliberately not clamped to the picture.** A pan that drags past the edge is still a
/// pan, so the gesture reads this raw; [`MapScreen::shows`] is what the readout asks. It is
/// written in `PostUpdate` because a node's place on the screen is a layout answer, and it
/// is a resource so that everything else can read a number instead of a layout.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
struct MapPointer(Option<Vec2>);

/// The world position one drag grabbed, held until the button comes up.
///
/// The world rather than the pointer's offset: the view is re-derived from it every frame,
/// so a drag cannot drift, and a centre that must be a whole block does not swallow a slow
/// drag one rounded fraction at a time.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
struct MapDrag(Option<Vec2>);

/// The full-screen overlay the map lives in.
#[derive(Component)]
struct MapRoot;

/// Where the player is standing, drawn over the picture and turned to face their heading.
#[derive(Component)]
struct PlayerDot;

/// The node the picture is drawn inside, and the one the pointer is measured against.
#[derive(Component)]
struct MapViewport;

/// The `ImageNode` itself, sized to the picture in logical screen pixels.
#[derive(Component)]
struct MapCanvas;

/// One line of the side panel, named by what it reads out.
///
/// One component with a variant rather than a marker each: the refresh below is then a
/// single query with a total match in it, so a readout added here without a line to fill
/// it is a build failure rather than a label that never changes.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum MapReading {
    /// What the pointer is over.
    Cursor,
    /// Where the player stands, from the server's own answer.
    You,
    /// How much world one map pixel covers.
    Scale,
}

/// The overlay's backdrop, shared with the inventory: one dimming for every full-screen
/// window, so a player never has to learn which screen dims how much.
const BACKDROP: Color = Color::srgba(0.012, 0.016, 0.024, 0.96);

/// Behind the picture, and visible only where the picture does not reach — the viewport is
/// not a whole number of map pixels wide. Darker than the fog so the edge of the drawn
/// sheet is a boundary rather than more nothing.
const VIEWPORT_BACKDROP: Color = Color::srgb(0.015, 0.018, 0.024);

/// The side panel's plate, the inventory window's colour.
const PANEL: Color = Color::srgb(0.075, 0.085, 0.105);

/// The side panel's width in logical pixels: wide enough for the longest readout a
/// six-figure coordinate can produce, and no wider.
const PANEL_WIDTH: f32 = 250.0;

/// The panel's readouts, and the heading over them.
const READING_SIZE: f32 = 16.0;
const TITLE_SIZE: f32 = 22.0;

/// A readout, dimmer than the heading.
const READING: Color = Color::srgb(0.78, 0.80, 0.84);

/// The player's dot, in logical pixels.
const DOT_SIZE: f32 = 10.0;

/// The needle that says which way they are facing: as long again as the dot is wide, and
/// narrow, so the heading reads at a glance without hiding the ground under it.
///
/// A needle rather than the wedge the issue asks for, and the reason is `bevy_ui`: it draws
/// rectangles, and the triangle a wedge needs would have to be a rotated node pretending or
/// an image nobody can read a heading off. This is the same information in the shape the
/// toolkit has.
const NEEDLE: Vec2 = Vec2::new(4.0, 12.0);

/// Keeps the map's viewport and its tile cache in step with the server.
pub(super) struct MapUiPlugin;

impl Plugin for MapUiPlugin {
    fn build(&self, app: &mut App) {
        // Initialised here as well as by their producers, which is what keeps this module
        // headlessly testable on its own — the same reason every other panel does it.
        // **`init_asset` is not idempotent**, and calling it after `ImagePlugin` replaces
        // the whole image store — `player/livery.rs` records what that cost and why the
        // guard is a guard rather than defensive programming. The real app has the store
        // from `DefaultPlugins`; a focused headless test brings its own `AssetPlugin`.
        if !app.world().contains_resource::<Assets<Image>>() {
            app.init_asset::<Image>();
        }
        // Initialised here as well as by their producers, which is what keeps this module
        // headlessly testable on its own — the same reason every other panel does it.
        app.init_resource::<MapScreen>()
            .init_resource::<MapTiles>()
            .init_resource::<MapInbox>()
            .init_resource::<Markers>()
            .init_resource::<InputMode>()
            .init_resource::<PlayerStats>()
            // `FromWorld`, so the handle exists before the `Startup` system that spawns
            // the node holding it — there is no ordering here for anybody to get wrong.
            .init_resource::<MapPicture>()
            .init_resource::<Painted>()
            .init_resource::<MapPointer>()
            .init_resource::<MapDrag>()
            .init_resource::<LookState>()
            .add_systems(Startup, spawn_map_screen)
            .add_systems(
                Update,
                (
                    // After the mode, because the frame `M` is pressed on is the frame
                    // the map opens — a screen that read yesterday's mode would open one
                    // frame late and ask for its first tiles one frame after that.
                    follow_input_mode.after(crate::player::ApplyInputMode),
                    // After the network, so a tile that arrived this frame is in the
                    // cache before the request pass decides it is missing.
                    ingest_map_payloads.after(crate::net::DrainNetwork),
                    // Before the asking, so a frame that panned asks for the squares the
                    // view it ended on overlaps rather than the one it started from.
                    drag_the_map,
                    zoom_the_map,
                    request_map_tiles,
                    // Last, so the picture drawn this frame is composed from the cache
                    // this frame ended with rather than the one it started on.
                    paint_the_map,
                    show_the_map,
                    refresh_the_panel,
                    place_the_player_dot,
                    // After the dot, because both are children of the picture and the
                    // picture is sized by the composition above them.
                    draw_the_marks,
                    hover_a_mark,
                )
                    .chain(),
            )
            // The viewport's size is a layout answer, so it can only be read after taffy
            // has written one — the same `PostUpdate` placement the crafting scrollbar
            // uses, and for the same reason.
            .add_systems(
                PostUpdate,
                (measure_the_viewport, follow_the_pointer).after(UiSystems::Layout),
            );
    }
}

/// Builds the overlay once: a backdrop, the viewport the picture is drawn in, and the
/// panel that reads out what the view is of.
fn spawn_map_screen(mut commands: Commands, picture: Res<MapPicture>) {
    commands
        .spawn((
            MapRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Stretch,
                column_gap: Val::Px(12.0),
                padding: UiRect::all(Val::Px(18.0)),
                ..default()
            },
            BackgroundColor(BACKDROP),
            Visibility::Hidden,
            GlobalZIndex(30),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    MapViewport,
                    Node {
                        // The viewport takes whatever the panel leaves, which is what
                        // makes its size a layout answer rather than a constant this
                        // module would have to keep in step with the window.
                        flex_grow: 1.0,
                        height: Val::Percent(100.0),
                        overflow: Overflow::clip(),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BackgroundColor(VIEWPORT_BACKDROP),
                    // **Block, and it is the one node here that does.** This is the
                    // surface a drag begins on; a `Pass` here would send the gesture to
                    // whatever is behind a full-screen overlay, which is the world.
                    FocusPolicy::Block,
                ))
                .with_children(|viewport| {
                    viewport
                        .spawn((
                            MapCanvas,
                            ImageNode::new(picture.0.clone()),
                            Node {
                                width: Val::Px(0.0),
                                height: Val::Px(0.0),
                                ..default()
                            },
                            // The picture is drawn on top of the viewport and must not take
                            // the pointer off it, or a drag would begin only in the margin the
                            // rounding leaves.
                            FocusPolicy::Pass,
                        ))
                        // A child of the picture and not of the viewport, so its place is the
                        // projection's own answer with nothing added: the picture is centred
                        // in a viewport it rarely divides exactly, and a dot measured from the
                        // viewport's corner would carry that leftover as an error.
                        .with_children(|canvas| {
                            canvas
                                .spawn((
                                    PlayerDot,
                                    Node {
                                        position_type: PositionType::Absolute,
                                        display: Display::None,
                                        width: Val::Px(0.0),
                                        height: Val::Px(0.0),
                                        align_items: AlignItems::Center,
                                        justify_content: JustifyContent::Center,
                                        ..default()
                                    },
                                    FocusPolicy::Pass,
                                ))
                                .with_children(|dot| {
                                    dot.spawn((
                                        Node {
                                            position_type: PositionType::Absolute,
                                            width: Val::Px(NEEDLE.x),
                                            height: Val::Px(NEEDLE.y),
                                            bottom: Val::Px(DOT_SIZE / 2.0),
                                            border_radius: BorderRadius::all(Val::Px(
                                                NEEDLE.x / 2.0,
                                            )),
                                            ..default()
                                        },
                                        BackgroundColor(super::SELECTED_EDGE),
                                        FocusPolicy::Pass,
                                    ));
                                    dot.spawn((
                                        Node {
                                            width: Val::Px(DOT_SIZE),
                                            height: Val::Px(DOT_SIZE),
                                            border_radius: BorderRadius::all(Val::Px(
                                                DOT_SIZE / 2.0,
                                            )),
                                            ..default()
                                        },
                                        BackgroundColor(super::SELECTED_EDGE),
                                        FocusPolicy::Pass,
                                    ));
                                });
                        });
                });

            overlay
                .spawn((
                    Node {
                        width: Val::Px(PANEL_WIDTH),
                        flex_shrink: 0.0,
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(10.0),
                        padding: UiRect::all(Val::Px(14.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(PANEL),
                    FocusPolicy::Pass,
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new("World map"),
                        TextFont {
                            font_size: FontSize::Px(TITLE_SIZE),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        FocusPolicy::Pass,
                    ));
                    panel.spawn((
                        MapReading::Cursor,
                        Text::new(cursor_reading(None)),
                        TextFont {
                            font_size: FontSize::Px(READING_SIZE),
                            ..default()
                        },
                        TextColor(READING),
                        FocusPolicy::Pass,
                    ));
                    panel.spawn((
                        MapReading::You,
                        Text::new(you_reading(None)),
                        TextFont {
                            font_size: FontSize::Px(READING_SIZE),
                            ..default()
                        },
                        TextColor(READING),
                        FocusPolicy::Pass,
                    ));
                    panel.spawn((
                        MapReading::Scale,
                        Text::new(scale_reading(MapScale::default())),
                        TextFont {
                            font_size: FontSize::Px(READING_SIZE),
                            ..default()
                        },
                        TextColor(READING),
                        FocusPolicy::Pass,
                    ));
                });

            // A child of the overlay rather than of the picture: it is anchored to the
            // window's own corners, and a node inside the clipped viewport could not reach
            // them. `GlobalZIndex(31)` is the shared bundle's, one above this overlay, so a
            // mark's tooltip is over the marks without any of them being over the panel.
            overlay.spawn(super::tooltip_bundle(MarkerTooltip));
        });
}

/// Puts the overlay up exactly while the map is open.
fn show_the_map(screen: Res<MapScreen>, mut roots: Query<&mut Visibility, With<MapRoot>>) {
    let wanted = if screen.is_open() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut roots {
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// Tells the viewport how big it turned out to be.
///
/// The window is the only thing that knows: the node grows into whatever the panel leaves,
/// and that number exists only after a layout pass. Until then [`DEFAULT_VIEWPORT`] stands,
/// which is what lets the asking half work on the first frame instead of waiting to be
/// drawn.
fn measure_the_viewport(
    viewports: Query<&ComputedNode, With<MapViewport>>,
    mut screen: ResMut<MapScreen>,
) {
    let Some(node) = viewports.iter().next() else {
        return;
    };
    // Logical pixels, because everything else on this screen is: `ComputedNode::size` is
    // physical, and its own inverse scale factor is what takes it back.
    let size = node.size() * node.inverse_scale_factor;
    if !size.x.is_finite() || !size.y.is_finite() || size.x < 1.0 || size.y < 1.0 {
        // A hidden overlay lays out at zero, and a viewport of nothing is not a view. The
        // last measurement stands, so closing the map does not throw away the size it will
        // reopen at.
        return;
    }
    let measured = size.as_uvec2();
    if screen.viewport != measured {
        screen.viewport = measured;
    }
}

/// Composes the picture whenever the view or the cache has moved, and never otherwise.
fn paint_the_map(
    screen: Res<MapScreen>,
    tiles: Res<MapTiles>,
    mut picture: ResMut<MapPicture>,
    mut painted: ResMut<Painted>,
    mut images: ResMut<Assets<Image>>,
    mut canvases: Query<(&mut Node, &mut ImageNode), With<MapCanvas>>,
) {
    if !screen.is_open() {
        return;
    }
    let wanted = Some((*screen, tiles.revision));
    // **Whether the picture is still there is part of the guard, not a step after it.**
    // `painted` records what was last composed, never that there is still somewhere to
    // compose into, so a store replaced under the app leaves the two describing different
    // worlds. Ask only whether the view moved and the answer on the frame after such a
    // replacement is "no": the map would keep the early return, never reach the store, and
    // stay blank for the rest of the session without one frame ever noticing.
    if painted.0 == wanted && images.contains(&picture.0) {
        return;
    }
    let composed = compose(&screen, &tiles);
    if let Some(mut slot) = images.get_mut(&picture.0) {
        *slot = composed;
    } else {
        // The handle is this resource's own and the store is the one it was minted from,
        // so an absent asset is a store that has been replaced under the app --
        // `player/livery.rs` records what that cost and why every `init_asset` here is
        // guarded. **Composing into the old handle again is not a recovery**: the id names
        // a slot in a store that no longer exists, so it would fail exactly as it just
        // did, every frame, for good. The picture is minted afresh into the store that is
        // actually there now and the node re-pointed at it.
        picture.0 = images.add(composed);
        for (_, mut node) in &mut canvases {
            node.image = picture.0.clone();
        }
    }
    painted.0 = wanted;

    // The node is sized in logical screen pixels and the image in map pixels: the zoom is
    // the whole of the difference, and it is applied here rather than in the composition
    // so that magnifying costs no texels.
    let drawn = screen.drawn_size().as_vec2();
    for (mut canvas, _) in &mut canvases {
        let (width, height) = (Val::Px(drawn.x), Val::Px(drawn.y));
        if canvas.width != width {
            canvas.width = width;
        }
        if canvas.height != height {
            canvas.height = height;
        }
    }
}

/// What the panel says about where the player is.
///
/// The same three numbers in the same order as the compass, from the same function, so the
/// line a player reads on the HUD and the line they read on the map cannot disagree. A
/// position the server has not sent yet says so rather than printing zeroes, which are a
/// real place and the one place somebody might actually be standing.
fn you_reading(position: Option<Vec3>) -> String {
    match position {
        Some(position) => format!("You {}", coordinates_reading(position)),
        None => "You not placed yet".to_owned(),
    }
}

/// What the panel says about how much world one map pixel covers.
fn scale_reading(scale: MapScale) -> String {
    match scale.blocks() {
        1 => "1 px = 1 block".to_owned(),
        blocks => format!("1 px = {blocks} blocks"),
    }
}

/// What the panel says about the block under the pointer.
///
/// **A pointer that is not over the map says so rather than naming a block.** The picture
/// does not fill the viewport exactly, and a readout that answered from the nearest edge
/// would be a coordinate a player could act on and nobody was pointing at.
fn cursor_reading(block: Option<IVec2>) -> String {
    match block {
        Some(block) => format!("Cursor X {} | Z {}", block.x, block.y),
        None => "Cursor off the map".to_owned(),
    }
}

/// Keeps the three readouts in step with the view, the pointer and the server's answer.
fn refresh_the_panel(
    screen: Res<MapScreen>,
    stats: Res<PlayerStats>,
    pointer: Res<MapPointer>,
    mut readings: Query<(&MapReading, &mut Text)>,
) {
    if !screen.is_open() {
        return;
    }
    let under = pointer
        .0
        .filter(|point| screen.shows(*point))
        .map(|point| screen.block_at(point));
    for (reading, mut text) in &mut readings {
        let next = match reading {
            MapReading::Cursor => cursor_reading(under),
            MapReading::You => you_reading(stats.position),
            MapReading::Scale => scale_reading(screen.scale),
        };
        if text.0 != next {
            text.0 = next;
        }
    }
}

/// Puts the pointer on the picture, in the picture's own pixels.
///
/// The one system here that reads a layout, which is why it runs in `PostUpdate` after
/// taffy has written one — the `sync_craft_scrollbar` placement, for the same reason.
/// `UiGlobalTransform` and not `GlobalTransform`: `bevy_ui` writes the first and leaves the
/// second at the identity, which `ui/character.rs` records putting every node at the
/// screen's corner.
fn follow_the_pointer(
    screen: Res<MapScreen>,
    windows: Query<&Window, With<PrimaryWindow>>,
    canvases: Query<(&ComputedNode, &UiGlobalTransform), With<MapCanvas>>,
    mut pointer: ResMut<MapPointer>,
) {
    let placed = screen.is_open().then_some(()).and_then(|()| {
        let cursor = windows.iter().next()?.cursor_position()?;
        let (node, transform) = canvases.iter().next()?;
        // Logical pixels on both sides: the cursor is one already, and the node's own
        // inverse scale factor is what takes its physical rect back to one.
        let scale = node.inverse_scale_factor;
        let size = node.size() * scale;
        // The transform's translation *is* the node's centre — see `ui/character.rs` — so
        // the corner is half a size away from it.
        let corner = transform.translation * scale - size / 2.0;
        Some(cursor - corner)
    });
    if pointer.0 != placed {
        pointer.0 = placed;
    }
}

/// Moves the view while the primary button is held, keeping the place that was grabbed
/// under the pointer.
///
/// The inventory's drag rule with the inventory's ending: the gesture lives only as long as
/// the button, and losing the pointer ends it, because a release outside the window is not
/// delivered on every platform.
fn drag_the_map(
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    pointer: Res<MapPointer>,
    mut drag: ResMut<MapDrag>,
    mut screen: ResMut<MapScreen>,
) {
    let held = buttons
        .as_deref()
        .is_some_and(|buttons| buttons.pressed(MouseButton::Left));
    let grabbing = held && screen.is_open();
    let Some(point) = pointer.0.filter(|_| grabbing) else {
        if drag.0.is_some() {
            drag.0 = None;
        }
        return;
    };
    // The first frame of a gesture is what decides the place; every frame after it only
    // answers where that place has to be now.
    let grabbed = match drag.0 {
        Some(world) => world,
        None => {
            let world = screen.world_at(point);
            drag.0 = Some(world);
            world
        }
    };
    let mut moved = *screen;
    moved.look_so_that(grabbed, point);
    if moved != *screen {
        *screen = moved;
    }
}

/// Steps the view along the zoom ladder with the wheel.
fn zoom_the_map(
    scroll: Option<Res<AccumulatedMouseScroll>>,
    pointer: Res<MapPointer>,
    mut screen: ResMut<MapScreen>,
) {
    if !screen.is_open() {
        return;
    }
    let Some(steps) = scroll
        .filter(|scroll| scroll.delta.y.is_finite() && scroll.delta.y != 0.0)
        .map(|scroll| scroll.delta.y.signum() as i32)
    else {
        return;
    };
    // A wheel turned while the pointer is beside the map still has somewhere to go, and
    // the middle of the picture is the only answer that is not a guess about intent.
    let point = pointer
        .0
        .filter(|point| screen.shows(*point))
        .unwrap_or_else(|| screen.drawn_size().as_vec2() / 2.0);
    let mut stepped = *screen;
    stepped.zoom_by(steps, point);
    if stepped != *screen {
        *screen = stepped;
    }
}

/// Puts the player's dot where the server says they are, facing where they are looking.
///
/// **The server's position and the client's heading**, which is not an inconsistency: this
/// client never sends its own place, so `PlayerStats::position` is the only answer to
/// *where*, while the yaw is the one thing in `player/` that is genuinely local. A position
/// the server has not sent, or one outside the picture, hides the dot rather than pinning
/// it to an edge it is not at. Party members are not drawn — the map is this character's.
fn place_the_player_dot(
    screen: Res<MapScreen>,
    stats: Res<PlayerStats>,
    look: Res<LookState>,
    mut dots: Query<(&mut Node, &mut UiTransform), With<PlayerDot>>,
) {
    let placed = stats
        .position
        .filter(|position| position.is_finite())
        .map(|position| screen.point_of(Vec2::new(position.x, position.z)))
        .filter(|point| screen.is_open() && screen.shows(*point));
    // The compass's own convention: a positive yaw turns west, so the bearing is the yaw
    // negated — and a `bevy_ui` rotation is applied in a space whose y grows downward, so
    // that same bearing is the clockwise turn a map needs.
    let facing = Rot2::radians(if look.yaw.is_finite() { -look.yaw } else { 0.0 });
    for (mut node, mut transform) in &mut dots {
        let wanted = match placed {
            Some(point) => (Display::Flex, Val::Px(point.x), Val::Px(point.y)),
            None => (Display::None, node.left, node.top),
        };
        if (node.display, node.left, node.top) != wanted {
            (node.display, node.left, node.top) = wanted;
        }
        if transform.rotation != facing {
            transform.rotation = facing;
        }
    }
}

/// Every mark this character holds, as the server last listed them.
///
/// **Replaced wholesale, never merged.** `MarkerList` is the complete list by contract, so
/// there is no revision to agree on and no delta to miss: the last list received *is* the
/// state, and a mark that has stopped appearing in it has stopped existing. That is also
/// what makes this the client's whole model of a mark -- nothing here is placed hopefully
/// and reconciled later, because there is no shape a reconciliation could take.
///
/// Per session, for the reason [`MapTiles`] is: a mark belongs to one character on one
/// world, and keeping it across a sign-in would draw the last character's finds over this
/// one's ground.
#[derive(Resource, Debug, Default, PartialEq, Eq)]
struct Markers(Vec<Marker>);

/// One mark drawn over the picture.
///
/// The kind is carried as well as the id so the reconcile below can tell a mark that moved
/// from one that has to be redrawn, without looking the mark up to find out.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerPin {
    marker_id: u64,
    kind: MarkerKind,
}

/// The map's single tooltip node, which says what the mark under the pointer is.
#[derive(Component)]
struct MarkerTooltip;

/// One mark's icon, in logical pixels. Big enough to read a silhouette off and small enough
/// that a cluster of marks is still ground rather than a wall of pictures.
const MARKER_SIZE: f32 = 22.0;

/// What one kind of mark is called on screen.
///
/// ASCII only, for the reason every string this client draws is: the embedded fallback font
/// is the whole font stack and a glyph it lacks renders as nothing at all.
const fn marker_label(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Resource => "Resource",
        MarkerKind::Cave => "Cave",
        MarkerKind::Monster => "Monster",
        MarkerKind::Boss => "Boss",
        MarkerKind::Camp => "Camp",
        MarkerKind::Village => "Village",
        MarkerKind::Note => "Note",
    }
}

/// What the tooltip says about one mark.
///
/// A mark with no note is its kind and nothing else, rather than a kind followed by an empty
/// box: an empty note is ordinary -- a pin on a cave is a complete thought -- and a
/// separator with nothing after it would read as text that failed to load.
///
/// `|` is the separator, the one the panel and the level readout already use. The typographic
/// alternatives are exactly the characters the font cannot draw.
fn marker_reading(marker: &Marker) -> String {
    match marker.note.trim() {
        "" => marker_label(marker.kind).to_owned(),
        note => format!("{} | {note}", marker_label(marker.kind)),
    }
}

/// Where one mark's icon sits on the picture, given the point its block projects to.
///
/// Centred on the point rather than hung from it, so the picture a player clicked toward is
/// the block the mark is on -- the dot beside it is centred the same way.
fn pin_node(point: Vec2) -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(point.x - MARKER_SIZE / 2.0),
        top: Val::Px(point.y - MARKER_SIZE / 2.0),
        width: Val::Px(MARKER_SIZE),
        height: Val::Px(MARKER_SIZE),
        ..default()
    }
}

/// Keeps exactly the marks the projection can show on the screen, where the projection puts
/// them.
///
/// **A mark outside the viewport is not spawned at all**, rather than spawned and hidden. A
/// character may hold sixty-four of them and a close zoom shows two; the ones off the edge
/// are not nodes waiting to be laid out, and the reconcile is what makes zooming out grow
/// the set rather than reveal it.
///
/// It runs when the list changes or the view moves, and on no other frame: the pins are
/// children of the picture, so a still map with an unchanged list has nothing to re-place.
///
/// **`Block`, unlike everything else drawn over the picture.** A pin has to take the pointer
/// or the tooltip below has nothing to read; the player's dot and the picture itself pass it
/// through, because a drag must begin anywhere on the map that is not a mark.
fn draw_the_marks(
    mut commands: Commands,
    screen: Res<MapScreen>,
    markers: Res<Markers>,
    canvases: Query<Entity, With<MapCanvas>>,
    mut pins: Query<(Entity, &MarkerPin, &mut Node)>,
) {
    if !screen.is_changed() && !markers.is_changed() {
        return;
    }
    let Some(canvas) = canvases.iter().next() else {
        return;
    };

    // A closed map draws none of them, which is also what empties the screen on a
    // disconnect: the session takes the list and the mode takes the window.
    let mut wanted: HashMap<u64, (MarkerKind, Vec2)> = HashMap::new();
    if screen.is_open() {
        for marker in &markers.0 {
            // The middle of the block, so a mark at the closest zoom sits on the square it
            // names rather than on its top-left corner.
            let point = screen.point_of(Vec2::new(marker.x as f32 + 0.5, marker.z as f32 + 0.5));
            if screen.shows(point) {
                wanted.insert(marker.marker_id, (marker.kind, point));
            }
        }
    }

    for (entity, pin, mut node) in &mut pins {
        // A kind that no longer matches is a redraw rather than a move. Unreachable while
        // the server mints an id once and never again, and one arm rather than an
        // assumption: the picture is the pin's, and nothing else here would notice.
        match wanted.remove(&pin.marker_id) {
            Some((kind, point)) if kind == pin.kind => {
                let placed = pin_node(point);
                if (node.left, node.top) != (placed.left, placed.top) {
                    (node.left, node.top) = (placed.left, placed.top);
                }
            }
            Some(again) => {
                wanted.insert(pin.marker_id, again);
                commands.entity(entity).despawn();
            }
            None => commands.entity(entity).despawn(),
        }
    }

    for (marker_id, (kind, point)) in wanted {
        commands
            .spawn((
                MarkerPin { marker_id, kind },
                ChildOf(canvas),
                Button,
                pin_node(point),
                FocusPolicy::Block,
            ))
            .with_children(|pin| super::icon::spawn_marker(pin, kind));
    }
}

/// Names the mark under the pointer, and nothing else.
///
/// **Display only, and structurally so**, exactly as `ui/inventory.rs`'s does: it writes no
/// message and touches no resource, so resting the pointer on a mark cannot become a
/// request. It is the same single-node mechanism too -- one tooltip for the screen, moved
/// and rewritten rather than spawned per mark.
///
/// The text comes from [`Markers`] rather than from the pin, because the note does: a pin
/// carries what it needs to be drawn, and the list is the state.
fn hover_a_mark(
    screen: Res<MapScreen>,
    markers: Res<Markers>,
    windows: Query<&Window, With<PrimaryWindow>>,
    pins: Query<(&Interaction, &MarkerPin)>,
    mut tooltips: Query<(&mut Node, &mut Text, &mut Visibility), With<MarkerTooltip>>,
) {
    let hovered = screen
        .is_open()
        .then(|| {
            pins.iter()
                // The same "any interaction at all" the inventory reads, so a mark held
                // down keeps the label it was hovered with.
                .find(|(interaction, _)| **interaction != Interaction::None)
                .map(|(_, pin)| pin.marker_id)
        })
        .flatten()
        .and_then(|marker_id| {
            markers
                .0
                .iter()
                .find(|marker| marker.marker_id == marker_id)
        })
        .map(marker_reading);

    let pointer = super::pointer_in_window(&windows);
    for (mut node, mut text, mut visibility) in &mut tooltips {
        let next = match hovered {
            // `Inherited` rather than `Visible`: the overlay owns whether the map is up at
            // all, and a tooltip must not survive it closing.
            Some(_) => Visibility::Inherited,
            None => Visibility::Hidden,
        };
        if *visibility != next {
            *visibility = next;
        }
        let reading = hovered.as_deref().unwrap_or("");
        if text.0 != reading {
            text.0 = reading.to_owned();
        }
        let Some((cursor, window)) = pointer.filter(|_| hovered.is_some()) else {
            continue;
        };
        super::anchor_for(cursor, window).apply_to(&mut node);
    }
}

/// Opens and closes the map with [`InputMode::Map`], and drops the cache with the session.
///
/// The mode is the single owner of *whether* the map is up — `ui/mod.rs` already refuses
/// the key while dead and forces the mode closed on death, so this system has no life
/// rule of its own and deliberately does not grow one.
fn follow_input_mode(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    stats: Res<PlayerStats>,
    mut screen: ResMut<MapScreen>,
    mut tiles: ResMut<MapTiles>,
    mut markers: ResMut<Markers>,
) {
    if session.is_none() {
        if screen.is_open() {
            screen.close();
        }
        // A tile is drawn for one character in one world. Keeping the cache across
        // sessions would put the last character's ground under this one's fog.
        if !tiles.tiles.is_empty() || !tiles.in_flight.is_empty() {
            tiles.clear();
        }
        // And a mark belongs to one character, which is the stronger of the two: a tile is
        // ground anybody could have walked over, and a mark is somebody's own note.
        if !markers.0.is_empty() {
            markers.0.clear();
        }
        return;
    }

    match (*mode == InputMode::Map, screen.is_open()) {
        (true, false) => {
            let centre = stats.position.map_or(IVec2::ZERO, |position| {
                IVec2::new(block_of(position.x), block_of(position.z))
            });
            screen.open(centre);
        }
        (false, true) => screen.close(),
        _ => {}
    }
}

/// Which block a world coordinate is in.
///
/// `floor` and not a truncating cast, for the reason `ui/compass.rs` gives: the two
/// disagree over exactly the half of the world that is negative. A non-finite coordinate
/// cannot reach a `Transform` — `net/codec.rs` refuses one — so the guard is a guard.
fn block_of(value: f32) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    value
        .floor()
        .clamp(-WORLD_EXTENT as f32, WORLD_EXTENT as f32) as i32
}

/// Files every drawn square, applies every ledger page and takes every mark list, in wire
/// order.
fn ingest_map_payloads(
    mut inbox: ResMut<MapInbox>,
    mut tiles: ResMut<MapTiles>,
    mut markers: ResMut<Markers>,
) {
    for event in inbox.take() {
        match event {
            MapEvent::Tile(tile) => tiles.insert(tile),
            MapEvent::Explored(explored) => {
                for column in explored.columns {
                    tiles.evict(column);
                }
            }
            // Assigned, never merged: the list is complete by contract, so the last one
            // received is the state. Two lists in one frame therefore leave the second,
            // which is the only reading of them that is not a guess about order.
            MapEvent::Markers(list) => {
                if markers.0 != list.markers {
                    markers.0 = list.markers;
                }
            }
        }
    }
}

/// Asks for the squares the viewport overlaps that this client does not hold.
fn request_map_tiles(
    time: Res<Time>,
    screen: Res<MapScreen>,
    session: Option<Res<Session>>,
    mut tiles: ResMut<MapTiles>,
    mut outbound: Option<ResMut<Outbound>>,
) {
    let now = time.elapsed();
    // An expired note is a request the server never answered — a spent token bucket says
    // nothing, so this is the only thing that ever makes the square askable again.
    tiles.in_flight.retain(|_, note| note.expires > now);

    if !screen.is_open() || session.is_none() {
        return;
    }
    let Some(outbound) = outbound.as_deref_mut() else {
        return;
    };

    for key in screen.tiles_in_view() {
        if tiles.in_flight.len() >= MAX_IN_FLIGHT {
            return;
        }
        if tiles.tiles.contains_key(&key) || tiles.in_flight.contains_key(&key) {
            continue;
        }
        let client_tick = tiles.next_tick;
        tiles.next_tick = tiles.next_tick.wrapping_add(1);
        outbound.send(encode_map_tile_request(&MapTileRequest {
            origin_x: key.origin_x,
            origin_z: key.origin_z,
            scale: key.scale.wire(),
            client_tick,
        }));
        tiles.in_flight.insert(
            key,
            InFlight {
                expires: now + REQUEST_RETRY,
                overtaken: None,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc::Receiver;

    use bevy::asset::AssetPlugin;

    use crate::net::{
        ANY_TOKEN, MAP_TILE_CELLS, MapExplored, MarkerList, SessionParams, map_tile_explored_bytes,
    };

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0, 70.0, 0.0],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: ANY_TOKEN,
        })
    }

    fn tile(origin_x: i32, origin_z: i32, scale: u8) -> MapTile {
        MapTile {
            origin_x,
            origin_z,
            scale,
            height: vec![64; MAP_TILE_CELLS],
            surface: vec![MapSurface::Grass; MAP_TILE_CELLS],
            explored: vec![0xff; map_tile_explored_bytes(scale).unwrap_or(1)],
        }
    }

    /// A map app with a viewport small enough that one screenful is a handful of squares.
    fn app() -> (App, Receiver<Vec<u8>>) {
        let (outbound, frames) = Outbound::to_a_test(64);
        let mut app = App::new();
        // `AssetPlugin` because the picture is an `Image` in a real store: `MapUiPlugin`
        // brings the store itself when nothing else has, but `init_asset` needs the
        // `AssetServer` this plugin is what supplies.
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .add_plugins(MapUiPlugin)
            .insert_resource(outbound)
            .insert_resource(session());
        (app, frames)
    }

    fn screen_of(app: &mut App) -> MapScreen {
        *app.world().resource::<MapScreen>()
    }

    fn requested(frames: &Receiver<Vec<u8>>) -> usize {
        frames.try_iter().count()
    }

    fn root_visibility(app: &mut App) -> Visibility {
        *app.world_mut()
            .query_filtered::<&Visibility, With<MapRoot>>()
            .iter(app.world())
            .next()
            .expect("the map overlay is spawned once at startup")
    }

    /// How the player's dot is currently drawn.
    fn dot(app: &mut App) -> (Display, Val, Val) {
        app.world_mut()
            .query_filtered::<&Node, With<PlayerDot>>()
            .iter(app.world())
            .next()
            .map(|node| (node.display, node.left, node.top))
            .expect("the dot is spawned with the picture")
    }

    /// What one of the side panel's readouts currently says.
    fn reading(app: &mut App, wanted: MapReading) -> String {
        app.world_mut()
            .query::<(&MapReading, &Text)>()
            .iter(app.world())
            .find(|(reading, _)| **reading == wanted)
            .map(|(_, text)| text.0.clone())
            .expect("a readout in the side panel")
    }

    #[test]
    fn the_map_opens_on_the_player_and_closes_with_the_mode() {
        let (mut app, _frames) = app();
        app.world_mut().resource_mut::<PlayerStats>().position =
            Some(Vec3::new(300.5, 70.0, -12.25));

        app.update();
        assert!(!screen_of(&mut app).is_open(), "playing draws no map");

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        let open = screen_of(&mut app);
        assert!(open.is_open());
        assert_eq!(open.centre, IVec2::new(300, -13), "floor, not truncate");
        assert_eq!(open.scale, MapScale::S4);
        assert_eq!(open.zoom, 2);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert!(!screen_of(&mut app).is_open());
    }

    #[test]
    fn a_viewport_asks_for_the_squares_it_overlaps_and_nothing_else() {
        let screen = MapScreen {
            open: true,
            centre: IVec2::new(0, 0),
            scale: MapScale::S4,
            zoom: 1,
            // 64 tile pixels at four blocks each is 256 blocks: exactly one tile span.
            viewport: UVec2::new(64, 64),
        };
        let tiles = screen.tiles_in_view();

        // Centred on the corner where four squares meet, a one-span viewport overlaps
        // all four of them and nothing further out.
        assert_eq!(tiles.len(), 4, "{tiles:?}");
        for tile in &tiles {
            assert!(matches!(tile.origin_x, -256 | 0), "{tile:?}");
            assert!(matches!(tile.origin_z, -256 | 0), "{tile:?}");
            assert_eq!(tile.scale, MapScale::S4);
        }
    }

    #[test]
    fn the_nearest_square_is_asked_for_first() {
        let screen = MapScreen {
            open: true,
            centre: IVec2::new(300, 300),
            scale: MapScale::S4,
            zoom: 1,
            viewport: UVec2::new(128, 128),
        };
        let tiles = screen.tiles_in_view();
        let nearest = tiles.first().expect("a viewport overlaps something");
        assert_eq!((nearest.origin_x, nearest.origin_z), (256, 256));
    }

    #[test]
    fn a_cached_square_is_not_asked_for_twice() {
        let (mut app, frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();

        let first = requested(&frames);
        assert!(first > 0, "an open map asks for what it does not hold");
        assert!(first <= MAX_IN_FLIGHT, "{first} requests went out at once");

        // Nothing has been answered and nothing has expired, so a second frame adds
        // nothing: the outstanding notes are what hold the squares.
        app.update();
        assert_eq!(requested(&frames), 0);
    }

    #[test]
    fn an_answered_square_is_kept_and_an_explored_column_throws_it_away() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();

        let key = TileKey {
            origin_x: 0,
            origin_z: 0,
            scale: MapScale::S4,
        };
        app.world_mut()
            .resource_mut::<MapInbox>()
            .push(MapEvent::Tile(tile(0, 0, 4)));
        app.update();
        assert!(app.world().resource::<MapTiles>().tiles.contains_key(&key));

        // Column (1, 1) is blocks 32..63 on both axes, which is inside the square at the
        // origin and outside every other one.
        app.world_mut()
            .resource_mut::<MapInbox>()
            .push(MapEvent::Explored(MapExplored {
                columns: vec![MapColumn { cx: 1, cz: 1 }],
            }));
        app.update();
        assert!(
            !app.world().resource::<MapTiles>().tiles.contains_key(&key),
            "a newly explored column makes its square stale"
        );
    }

    #[test]
    fn an_answer_the_ledger_has_overtaken_is_thrown_away() {
        let (mut app, frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        assert!(
            requested(&frames) > 0,
            "an open map asks for what it does not hold"
        );

        let key = TileKey {
            origin_x: 0,
            origin_z: 0,
            scale: MapScale::S4,
        };
        assert!(
            app.world()
                .resource::<MapTiles>()
                .in_flight
                .contains_key(&key),
            "the square under the player is one of the first asked for"
        );

        // The page arrives while that square's request is still outstanding. The note is
        // what holds the square, so nothing is asked for a second time.
        app.world_mut()
            .resource_mut::<MapInbox>()
            .push(MapEvent::Explored(MapExplored {
                columns: vec![MapColumn { cx: 1, cz: 1 }],
            }));
        app.update();
        assert_eq!(
            requested(&frames),
            0,
            "a square with an answer in the mail is not asked for again"
        );

        // An answer drawn before that column was explored says so in its own mask.
        let mut overtaken = tile(0, 0, 4);
        overtaken.explored = vec![0; map_tile_explored_bytes(4).unwrap_or(1)];
        app.world_mut()
            .resource_mut::<MapInbox>()
            .push(MapEvent::Tile(overtaken));
        app.update();
        assert!(
            !app.world().resource::<MapTiles>().tiles.contains_key(&key),
            "a tile drawn without the explored column is the stale square the page evicted"
        );
        assert_eq!(
            requested(&frames),
            1,
            "and dropping its note is what offers the square again"
        );

        // One drawn with the column explored is the answer the page was waiting for.
        app.world_mut()
            .resource_mut::<MapInbox>()
            .push(MapEvent::Tile(tile(0, 0, 4)));
        app.update();
        assert!(app.world().resource::<MapTiles>().tiles.contains_key(&key));
    }

    #[test]
    fn a_tile_carries_the_ledger_it_was_drawn_from() {
        let mut drawn = tile(0, 0, 4);
        // A scale-4 tile covers eight chunk columns each way, one bit each, row-major
        // with z outer: column (1, 1) is bit nine.
        drawn.explored = vec![0; map_tile_explored_bytes(4).unwrap_or(1)];
        drawn.explored[1] = 0b0000_0010;
        assert_eq!(drawn_with(&drawn, MapColumn { cx: 1, cz: 1 }), Some(true));
        assert_eq!(drawn_with(&drawn, MapColumn { cx: 0, cz: 0 }), Some(false));
        assert_eq!(
            drawn_with(&drawn, MapColumn { cx: 8, cz: 0 }),
            None,
            "next square"
        );

        let negative = tile(-256, -256, 4);
        assert_eq!(
            drawn_with(&negative, MapColumn { cx: -8, cz: -8 }),
            Some(true)
        );
        assert_eq!(drawn_with(&negative, MapColumn { cx: -9, cz: -8 }), None);
    }

    #[test]
    fn a_whole_screenful_is_asked_for_at_the_closest_zoom() {
        let screen = MapScreen {
            open: true,
            centre: IVec2::ZERO,
            scale: MapScale::S4,
            zoom: 1,
            viewport: DEFAULT_VIEWPORT,
        };
        // 1024 by 768 tile pixels over 64-pixel squares, and the default centre sits on
        // a corner: sixteen squares by twelve, every one of them asked for.
        assert_eq!(screen.tiles_in_view().len(), 16 * 12);
    }

    #[test]
    fn a_viewport_no_window_could_have_is_asked_nothing() {
        let screen = MapScreen {
            open: true,
            centre: IVec2::ZERO,
            scale: MapScale::S4,
            zoom: 1,
            viewport: UVec2::splat(u32::MAX),
        };
        assert!(
            screen.tiles_in_view().is_empty(),
            "a grid past the guard is a layout bug, and it allocates nothing"
        );
    }

    #[test]
    fn a_column_outside_a_square_leaves_it_alone() {
        let key = TileKey {
            origin_x: 0,
            origin_z: 0,
            scale: MapScale::S4,
        };
        assert!(key.contains(MapColumn { cx: 0, cz: 0 }));
        assert!(key.contains(MapColumn { cx: 7, cz: 7 }), "block 224..255");
        assert!(!key.contains(MapColumn { cx: 8, cz: 0 }), "block 256 on");
        assert!(!key.contains(MapColumn { cx: -1, cz: 0 }), "block -32..-1");
    }

    #[test]
    fn a_square_the_server_never_answered_is_asked_for_again() {
        let (mut app, frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        let first = requested(&frames);
        assert!(first > 0);

        // A spent token bucket is silence, so the only thing that frees the square is the
        // note expiring. Retiring the deadlines is what stands in for a clock that moved.
        for note in app
            .world_mut()
            .resource_mut::<MapTiles>()
            .in_flight
            .values_mut()
        {
            note.expires = Duration::ZERO;
        }
        app.update();
        assert_eq!(
            requested(&frames),
            first,
            "the same squares are offered again once their requests expire"
        );
    }

    #[test]
    fn the_cache_does_not_outlive_the_session() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        app.world_mut()
            .resource_mut::<MapInbox>()
            .push(MapEvent::Tile(tile(0, 0, 4)));
        app.update();
        assert!(!app.world().resource::<MapTiles>().tiles.is_empty());

        app.world_mut().remove_resource::<Session>();
        app.update();
        assert!(app.world().resource::<MapTiles>().tiles.is_empty());
        assert!(!screen_of(&mut app).is_open());
    }

    /// The bytes one texel of the composed picture carries.
    fn texel(image: &Image, column: u32, row: u32) -> [u8; 4] {
        let width = image.texture_descriptor.size.width;
        let data = image
            .data
            .as_ref()
            .expect("a composed picture carries data");
        let at = ((row * width + column) * 4) as usize;
        [data[at], data[at + 1], data[at + 2], data[at + 3]]
    }

    /// One colour as the picture stores it: opaque, because a map has nothing behind it.
    fn opaque(colour: [u8; 3]) -> [u8; 4] {
        [colour[0], colour[1], colour[2], u8::MAX]
    }

    fn fog() -> [u8; 4] {
        opaque(FOG.map(|channel| (channel * 255.0).round() as u8))
    }

    /// A sixteen-pixel view whose top-left pixel is block `(0, 0)`.
    fn tiny_view() -> MapScreen {
        MapScreen {
            open: true,
            // 16 pixels at four blocks each is 64 blocks, so a centre of 32 puts the
            // picture's corner on the origin.
            centre: IVec2::splat(32),
            scale: MapScale::S4,
            zoom: 1,
            viewport: UVec2::splat(16),
        }
    }

    #[test]
    fn a_square_nobody_holds_is_drawn_as_fog() {
        let picture = compose(&tiny_view(), &MapTiles::default());
        assert_eq!(picture.texture_descriptor.size.width, 16);
        assert_eq!(picture.texture_descriptor.size.height, 16);
        for (column, row) in [(0, 0), (15, 15), (7, 3)] {
            assert_eq!(texel(&picture, column, row), fog(), "at {column},{row}");
        }
    }

    #[test]
    fn an_explored_pixel_wears_its_surface_and_the_rest_is_fog() {
        let mut drawn = tile(0, 0, 4);
        drawn.explored = vec![0; map_tile_explored_bytes(4).unwrap_or(1)];
        // Only chunk column (0, 0) — blocks 0..31 on both axes, which is the first eight
        // pixels of the first eight rows at four blocks a pixel.
        drawn.explored[0] = 0b0000_0001;
        drawn.surface[0] = MapSurface::Water;
        let mut tiles = MapTiles::default();
        tiles.insert(drawn);

        let picture = compose(&tiny_view(), &tiles);
        assert_eq!(texel(&picture, 0, 0), opaque(shaded(MapSurface::Water, 64)));
        assert_eq!(texel(&picture, 1, 0), opaque(shaded(MapSurface::Grass, 64)));
        assert_eq!(texel(&picture, 7, 7), opaque(shaded(MapSurface::Grass, 64)));
        // Block 32 across and block 32 down are the next chunk column each way, and the
        // ledger this tile was drawn from had not reached either.
        assert_eq!(texel(&picture, 8, 0), fog(), "one column east");
        assert_eq!(texel(&picture, 0, 8), fog(), "one column south");
    }

    #[test]
    fn higher_ground_is_lighter_and_the_unnamed_wears_a_colour_of_its_own() {
        let low = shaded(MapSurface::Grass, 20);
        let high = shaded(MapSurface::Grass, 220);
        assert!(high[1] > low[1], "{high:?} is not lighter than {low:?}");

        let named = [
            MapSurface::Grass,
            MapSurface::Snow,
            MapSurface::Sand,
            MapSurface::Stone,
            MapSurface::Gravel,
            MapSurface::Water,
            MapSurface::Ice,
            MapSurface::Forest,
            MapSurface::Cave,
            MapSurface::Settlement,
        ];
        for surface in named {
            assert_ne!(
                surface_tint(MapSurface::Unknown),
                surface_tint(surface),
                "a surface this build cannot name would be read as {surface:?}"
            );
        }
    }

    #[test]
    fn the_overlay_and_its_readouts_follow_the_open_map() {
        let (mut app, _frames) = app();
        app.world_mut().resource_mut::<PlayerStats>().position = Some(Vec3::new(12.5, 64.0, -3.5));
        app.update();
        assert_eq!(root_visibility(&mut app), Visibility::Hidden);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        assert_eq!(root_visibility(&mut app), Visibility::Visible);
        assert_eq!(
            reading(&mut app, MapReading::You),
            "You X 12 | Z -4 | alt 64"
        );
        assert_eq!(reading(&mut app, MapReading::Scale), "1 px = 4 blocks");

        // The picture is the viewport divided by the zoom, and the node it is drawn into
        // is that many pixels magnified by the zoom again.
        let screen = screen_of(&mut app);
        assert_eq!(screen.image_size(), UVec2::new(512, 384));
        assert_eq!(screen.drawn_size(), UVec2::new(1024, 768));
        let handle = app.world().resource::<MapPicture>().0.clone();
        let images = app.world().resource::<Assets<Image>>();
        let picture = images.get(&handle).expect("the map's own picture");
        assert_eq!(picture.texture_descriptor.size.width, 512);
        assert_eq!(picture.texture_descriptor.size.height, 384);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert_eq!(root_visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn the_picture_is_composed_when_something_moves_and_not_every_frame() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        let first = *app.world().resource::<Painted>();
        assert!(first.0.is_some(), "an open map composes its picture");

        // A frame in which nothing arrived: `request_map_tiles` retires notes through a
        // `ResMut` every frame, so Bevy's change detection would say the cache moved.
        app.update();
        assert_eq!(*app.world().resource::<Painted>(), first, "nothing moved");

        app.world_mut()
            .resource_mut::<MapInbox>()
            .push(MapEvent::Tile(tile(0, 0, 4)));
        app.update();
        assert_ne!(
            *app.world().resource::<Painted>(),
            first,
            "a drawn square is something new to draw"
        );
    }

    /// What the canvas node is actually drawing.
    fn canvas_image(app: &mut App) -> Handle<Image> {
        app.world_mut()
            .query_filtered::<&ImageNode, With<MapCanvas>>()
            .iter(app.world())
            .next()
            .map(|node| node.image.clone())
            .expect("the map canvas is spawned once at startup")
    }

    #[test]
    fn a_picture_the_store_lost_is_minted_again_rather_than_left_blank() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        let first = app.world().resource::<MapPicture>().0.clone();
        assert!(
            app.world().resource::<Assets<Image>>().contains(&first),
            "an open map composes its picture"
        );

        // The whole image store replaced under the app: the hazard `player/livery.rs`
        // records, and the one thing that makes the map's own handle name nothing. Not one
        // number the view is composed from moved, so a guard that asks only whether the
        // view moved answers "no" on this frame and on every frame after it.
        app.world_mut().insert_resource(Assets::<Image>::default());
        app.update();

        // Not `assert_ne!` on the handle: a fresh store hands out the same index again, so
        // what proves the recovery is that the picture is *in the store that is there now*
        // -- under a guard that latches, this store is still empty and the `expect` fires.
        let second = app.world().resource::<MapPicture>().0.clone();
        let picture = app
            .world()
            .resource::<Assets<Image>>()
            .get(&second)
            .cloned()
            .expect("the map draws again rather than staying blank for the session");
        assert_eq!(picture.texture_descriptor.size.width, 512);
        assert_eq!(
            canvas_image(&mut app),
            second,
            "the node draws the picture that exists, not the one that was lost"
        );
    }

    /// A view big enough to drag around, at the rung the map opens on.
    fn wide_view() -> MapScreen {
        MapScreen {
            open: true,
            centre: IVec2::new(1000, -400),
            scale: MapScale::S4,
            zoom: 2,
            viewport: UVec2::new(800, 600),
        }
    }

    #[test]
    fn a_world_position_projects_to_a_point_and_back() {
        let screen = wide_view();
        for point in [Vec2::ZERO, Vec2::new(123.0, 47.0), Vec2::new(799.0, 599.0)] {
            let world = screen.world_at(point);
            assert!(
                screen.point_of(world).distance(point) < 0.001,
                "{point} came back as {}",
                screen.point_of(world)
            );
        }
        // The block under a point is the world position floored, on both sides of the
        // origin, which is the half a truncating cast gets wrong.
        let screen = MapScreen {
            centre: IVec2::ZERO,
            zoom: 1,
            viewport: UVec2::splat(16),
            ..wide_view()
        };
        assert_eq!(screen.block_at(Vec2::ZERO), IVec2::splat(-32));
        assert_eq!(screen.block_at(Vec2::new(8.0, 8.0)), IVec2::ZERO);
    }

    #[test]
    fn a_drag_keeps_the_place_it_grabbed_under_the_pointer() {
        let mut screen = wide_view();
        let grabbed_at = Vec2::new(300.0, 220.0);
        let grabbed = screen.world_at(grabbed_at);

        // A gesture is re-answered from where it began, so the intermediate steps are not
        // a sequence that can drift: each of these is the whole drag so far.
        for step in [
            Vec2::new(1.0, 0.0),
            Vec2::new(-37.5, 12.5),
            Vec2::new(0.0, 0.5),
        ] {
            let now = grabbed_at + step;
            screen.look_so_that(grabbed, now);
            assert!(
                screen.world_at(now).distance(grabbed) < 2.0,
                "{step} lost the place: {} is not {grabbed}",
                screen.world_at(now)
            );
        }
    }

    #[test]
    fn a_zoom_step_keeps_the_pointers_world_position_under_the_pointer() {
        let under = Vec2::new(120.0, 470.0);
        for steps in [1, -1, 2, -2] {
            let mut screen = wide_view();
            let before = screen.world_at(under);
            screen.zoom_by(steps, under);
            assert_ne!(
                (screen.scale, screen.zoom),
                (MapScale::S4, 2),
                "{steps} steps moved nothing"
            );
            let after = screen.world_at(under);
            assert!(
                after.distance(before) < 2.0,
                "{steps} steps moved {before} to {after}"
            );
        }
    }

    #[test]
    fn the_zoom_ladder_runs_from_end_to_end_and_stops() {
        let mut screen = wide_view();
        assert_eq!(screen.rung(), DEFAULT_RUNG);

        screen.zoom_by(99, Vec2::ZERO);
        assert_eq!((screen.scale, screen.zoom), (MapScale::S1, 4), "closest");
        screen.zoom_by(1, Vec2::ZERO);
        assert_eq!(
            (screen.scale, screen.zoom),
            (MapScale::S1, 4),
            "the end is an end, not a wrap"
        );

        screen.zoom_by(-99, Vec2::ZERO);
        assert_eq!((screen.scale, screen.zoom), (MapScale::S16, 1), "widest");
        screen.zoom_by(-1, Vec2::ZERO);
        assert_eq!((screen.scale, screen.zoom), (MapScale::S16, 1));
    }

    #[test]
    fn the_cursor_names_a_block_only_while_one_is_under_it() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        assert_eq!(reading(&mut app, MapReading::Cursor), "Cursor off the map");

        // The picture is 1024 by 768 logical pixels at the default view, and its top-left
        // pixel is the block the projection says it is.
        let screen = screen_of(&mut app);
        let corner = screen.origin_block();
        app.world_mut().resource_mut::<MapPointer>().0 = Some(Vec2::ZERO);
        app.update();
        assert_eq!(
            reading(&mut app, MapReading::Cursor),
            format!("Cursor X {} | Z {}", corner.x, corner.y)
        );

        // Beside the picture rather than on it: the readout says so instead of naming the
        // nearest block.
        app.world_mut().resource_mut::<MapPointer>().0 = Some(Vec2::new(-4.0, 20.0));
        app.update();
        assert_eq!(reading(&mut app, MapReading::Cursor), "Cursor off the map");
    }

    /// The pointer is measured in the picture's own logical pixels, on a display where a
    /// logical pixel is not a physical one.
    ///
    /// **The test the review of this pull request earned.** `bevy_ui` lays out in physical
    /// pixels: `ComputedNode::size` says so in its own doc, `UiGlobalTransform` is built
    /// from the same taffy answer, and `ComputedNode::inverse_scale_factor` is documented
    /// as what multiplies one back to logical -- which is what `bevy_ui`'s own
    /// `widget::viewport` does to put a cursor inside a node. `Window::cursor_position` is
    /// logical, and `measure_the_viewport` measures the viewport logically too, so the
    /// conversion is what makes the two ends the same units. Dropping it reads as correct
    /// at scale factor 1 and is wrong at every other, which is why the case is pinned at 2.
    #[test]
    fn the_pointer_is_logical_where_a_logical_pixel_is_not_a_physical_one() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;

        // 1600x1200 physical at scale factor 2 is 800x600 logical.
        let window = app
            .world_mut()
            .spawn((
                Window {
                    resolution: bevy::window::WindowResolution::new(1600, 1200)
                        .with_scale_factor_override(2.0),
                    ..default()
                },
                PrimaryWindow,
            ))
            .id();
        app.update();

        // A canvas 512x384 logical, centred at (400, 300) logical. Every number the layout
        // writes is twice that, and it is the layout's numbers that go in.
        let canvas = app
            .world_mut()
            .query_filtered::<Entity, With<MapCanvas>>()
            .iter(app.world())
            .next()
            .expect("the map canvas is spawned once at startup");
        app.world_mut().entity_mut(canvas).insert((
            ComputedNode {
                size: Vec2::new(1024.0, 768.0),
                inverse_scale_factor: 0.5,
                ..ComputedNode::DEFAULT
            },
            UiGlobalTransform::from_xy(800.0, 600.0),
        ));
        app.world_mut()
            .get_mut::<Window>(window)
            .expect("the window this test spawned")
            .set_cursor_position(Some(Vec2::new(500.0, 400.0)));
        app.update();

        // The canvas's logical top-left corner is (400 - 256, 300 - 192) = (144, 108).
        // Reading the layout as logical instead would put the corner at (288, 216) and the
        // pointer a quarter of the canvas away from where it is.
        assert_eq!(
            app.world().resource::<MapPointer>().0,
            Some(Vec2::new(356.0, 292.0)),
            "the pointer is not in the picture's own pixels"
        );
    }

    #[test]
    fn the_dot_waits_for_the_server_to_say_where_the_player_is() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        assert_eq!(dot(&mut app).0, Display::None, "nobody has been placed");

        // The map opens on the player, so a map reopened once the server has said where
        // they are puts them in the middle of the picture.
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        app.world_mut().resource_mut::<PlayerStats>().position = Some(Vec3::new(8.0, 70.0, 8.0));
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        let (display, left, top) = dot(&mut app);
        assert_eq!(display, Display::Flex);
        let middle = screen_of(&mut app).drawn_size().as_vec2() / 2.0;
        assert_eq!(left, Val::Px(middle.x));
        assert_eq!(top, Val::Px(middle.y));

        // Somewhere the picture does not reach: hidden rather than pinned to an edge the
        // player is not standing on.
        app.world_mut().resource_mut::<PlayerStats>().position =
            Some(Vec3::new(200_000.0, 70.0, 8.0));
        app.update();
        assert_eq!(dot(&mut app).0, Display::None);
    }

    fn mark(marker_id: u64, x: i32, z: i32, kind: MarkerKind, note: &str) -> Marker {
        Marker {
            marker_id,
            x,
            z,
            kind,
            note: note.to_owned(),
        }
    }

    /// Hands the screen one complete list, the way the server does.
    fn list(app: &mut App, markers: Vec<Marker>) {
        app.world_mut()
            .resource_mut::<MapInbox>()
            .push(MapEvent::Markers(MarkerList { markers }));
        app.update();
    }

    /// Every mark currently drawn, by id, with where its icon sits and how many rectangles
    /// it drew.
    fn drawn_marks(app: &mut App) -> Vec<(u64, MarkerKind, Val, Val, usize)> {
        let mut drawn: Vec<_> = app
            .world_mut()
            .query::<(&MarkerPin, &Node, Option<&Children>)>()
            .iter(app.world())
            .map(|(pin, node, children)| {
                (
                    pin.marker_id,
                    pin.kind,
                    node.left,
                    node.top,
                    children.map_or(0, |children| children.iter().count()),
                )
            })
            .collect();
        drawn.sort_by_key(|drawn| drawn.0);
        drawn
    }

    /// What the map's tooltip currently says, and whether it is up.
    fn tooltip(app: &mut App) -> (String, Visibility) {
        app.world_mut()
            .query_filtered::<(&Text, &Visibility), With<MarkerTooltip>>()
            .iter(app.world())
            .next()
            .map(|(text, visibility)| (text.0.clone(), *visibility))
            .expect("the map spawns one tooltip at startup")
    }

    /// Where the picture puts the middle of one block.
    fn pin_at(screen: &MapScreen, x: i32, z: i32) -> (Val, Val) {
        let point = screen.point_of(Vec2::new(x as f32 + 0.5, z as f32 + 0.5));
        (
            Val::Px(point.x - MARKER_SIZE / 2.0),
            Val::Px(point.y - MARKER_SIZE / 2.0),
        )
    }

    #[test]
    fn every_mark_in_the_list_is_drawn_where_the_projection_puts_it_and_no_others_are() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        assert!(drawn_marks(&mut app).is_empty(), "nothing has been listed");

        list(
            &mut app,
            vec![
                mark(1, 0, 0, MarkerKind::Cave, "cold"),
                mark(2, 64, -32, MarkerKind::Boss, ""),
                // Far outside a viewport that spans a couple of thousand blocks.
                mark(3, 900_000, 0, MarkerKind::Note, "another day"),
            ],
        );

        let screen = screen_of(&mut app);
        let drawn = drawn_marks(&mut app);
        assert_eq!(
            drawn.len(),
            2,
            "a mark off the edge of the viewport is not a node, {drawn:?}"
        );
        assert_eq!(drawn[0].0, 1);
        assert_eq!(drawn[0].1, MarkerKind::Cave);
        assert_eq!((drawn[0].2, drawn[0].3), pin_at(&screen, 0, 0));
        assert_eq!((drawn[1].2, drawn[1].3), pin_at(&screen, 64, -32));
        for mark in &drawn {
            assert!(
                mark.4 > 0,
                "a mark with no rectangles is not drawn: {mark:?}"
            );
        }
    }

    #[test]
    fn a_later_list_replaces_the_one_before_it_rather_than_adding_to_it() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();

        list(
            &mut app,
            vec![
                mark(1, 0, 0, MarkerKind::Cave, ""),
                mark(2, 64, 64, MarkerKind::Camp, ""),
            ],
        );
        assert_eq!(drawn_marks(&mut app).len(), 2);

        // The second mark is gone and the first has moved: both are the same message, and
        // neither is a delta this client had to work out.
        list(&mut app, vec![mark(1, 128, 128, MarkerKind::Cave, "")]);
        let screen = screen_of(&mut app);
        let drawn = drawn_marks(&mut app);
        assert_eq!(drawn.len(), 1, "the list is the whole state: {drawn:?}");
        assert_eq!(drawn[0].0, 1);
        assert_eq!((drawn[0].2, drawn[0].3), pin_at(&screen, 128, 128));
    }

    #[test]
    fn a_mark_the_view_left_behind_stops_being_a_node_and_comes_back_when_it_returns() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        list(&mut app, vec![mark(1, 0, 0, MarkerKind::Village, "")]);
        assert_eq!(drawn_marks(&mut app).len(), 1);

        // Two screenfuls away, with the list untouched.
        let span = screen_of(&mut app).span_blocks();
        app.world_mut().resource_mut::<MapScreen>().centre.x += span.x * 2;
        app.update();
        assert!(
            drawn_marks(&mut app).is_empty(),
            "a mark off the picture is not held as a hidden node"
        );

        app.world_mut().resource_mut::<MapScreen>().centre.x -= span.x * 2;
        app.update();
        assert_eq!(drawn_marks(&mut app).len(), 1, "and it comes back");
    }

    #[test]
    fn resting_the_pointer_on_a_mark_names_it_and_reads_out_its_note() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.world_mut().spawn((
            Window {
                resolution: bevy::window::WindowResolution::new(800, 600),
                ..default()
            },
            PrimaryWindow,
        ));
        app.update();
        list(
            &mut app,
            vec![
                mark(1, 0, 0, MarkerKind::Cave, "  bats, and a way down  "),
                mark(2, 64, 64, MarkerKind::Boss, ""),
            ],
        );
        assert_eq!(
            tooltip(&mut app).1,
            Visibility::Hidden,
            "nothing is hovered"
        );

        let pins: Vec<(Entity, u64)> = app
            .world_mut()
            .query::<(Entity, &MarkerPin)>()
            .iter(app.world())
            .map(|(entity, pin)| (entity, pin.marker_id))
            .collect();
        let noted = pins
            .iter()
            .find(|(_, marker_id)| *marker_id == 1)
            .expect("the noted mark is drawn")
            .0;
        let bare = pins
            .iter()
            .find(|(_, marker_id)| *marker_id == 2)
            .expect("the bare mark is drawn")
            .0;

        *app.world_mut()
            .get_mut::<Interaction>(noted)
            .expect("a mark takes the pointer") = Interaction::Hovered;
        app.update();
        assert_eq!(
            tooltip(&mut app),
            (
                "Cave | bats, and a way down".to_owned(),
                Visibility::Inherited
            )
        );

        *app.world_mut()
            .get_mut::<Interaction>(noted)
            .expect("a mark takes the pointer") = Interaction::None;
        *app.world_mut()
            .get_mut::<Interaction>(bare)
            .expect("a mark takes the pointer") = Interaction::Pressed;
        app.update();
        assert_eq!(
            tooltip(&mut app),
            ("Boss".to_owned(), Visibility::Inherited),
            "a mark with no note is its kind and nothing else"
        );

        // And the tooltip is anchored away from the pointer, through the same mechanism the
        // inventory's is.
        let node = app
            .world_mut()
            .query_filtered::<&Node, With<MarkerTooltip>>()
            .iter(app.world())
            .next()
            .expect("the map's tooltip")
            .clone();
        assert_eq!(node.position_type, PositionType::Absolute);
    }

    #[test]
    fn the_marks_do_not_outlive_the_session() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        list(&mut app, vec![mark(1, 0, 0, MarkerKind::Monster, "trolls")]);
        assert_eq!(drawn_marks(&mut app).len(), 1);

        app.world_mut().remove_resource::<Session>();
        app.update();
        app.update();
        assert!(app.world().resource::<Markers>().0.is_empty());
        assert!(
            drawn_marks(&mut app).is_empty(),
            "another character's finds do not survive the sign-out"
        );
    }

    #[test]
    fn a_closed_map_draws_no_marks() {
        let (mut app, _frames) = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Map;
        app.update();
        list(&mut app, vec![mark(1, 0, 0, MarkerKind::Resource, "iron")]);
        assert_eq!(drawn_marks(&mut app).len(), 1);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        app.update();
        assert!(drawn_marks(&mut app).is_empty());
        assert_eq!(
            app.world().resource::<Markers>().0.len(),
            1,
            "the list is the server's and outlives the window that draws it"
        );
    }

    /// Every kind is named, and no two share a name.
    ///
    /// The counterpart to the icon sweep: a kind that fell through to another kind's label
    /// would draw its own picture over somebody else's word.
    #[test]
    fn every_kind_of_mark_has_a_name_of_its_own() {
        for (index, kind) in MarkerKind::ALL.iter().enumerate() {
            assert!(!marker_label(*kind).is_empty(), "{kind:?} has no name");
            for other in &MarkerKind::ALL[index + 1..] {
                assert_ne!(
                    marker_label(*kind),
                    marker_label(*other),
                    "{kind:?} and {other:?} are called the same thing"
                );
            }
        }
    }

    #[test]
    fn a_closed_map_asks_for_nothing() {
        let (mut app, frames) = app();
        app.update();
        app.update();
        assert_eq!(requested(&frames), 0);
    }
}
