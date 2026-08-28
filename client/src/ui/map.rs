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

use std::collections::HashMap;
use std::time::Duration;

use bevy::prelude::*;

use crate::net::{
    CHUNK_COLUMN_BLOCKS, MAP_TILE_EDGE, MapColumn, MapEvent, MapInbox, MapTile, MapTileRequest,
    Outbound, Session, encode_map_tile_request, map_tile_span,
};
use crate::player::{InputMode, PlayerStats};

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

/// The most squares one viewport may be broken into.
///
/// A guard rather than a rule: the viewport is this client's own number, so nothing
/// hostile can drive it. What it stops is a mistake in the layout half producing an
/// allocation proportional to a garbage size.
const MAX_TILES_IN_VIEW: usize = 64;

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
        let mut tiles = Vec::new();
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
        tiles.truncate(MAX_TILES_IN_VIEW);
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
    /// When each outstanding request stops holding its square, on `Time`'s clock.
    ///
    /// A deadline rather than a departure time, so the expiry is one comparison and a
    /// test can retire a note by writing a number instead of moving a clock.
    in_flight: HashMap<TileKey, Duration>,
    /// Ordering and staleness for the server, never a clock. See `MapTileRequest`.
    next_tick: u32,
}

impl MapTiles {
    /// Records one drawn square. A tile the client never asked for is kept all the same —
    /// the server is the authority on what it sends, and a square is a square.
    fn insert(&mut self, tile: MapTile) {
        let Some(key) = key_of(&tile) else {
            return;
        };
        self.in_flight.remove(&key);
        self.tiles.insert(key, tile);
    }

    /// Throws away every square `column` falls inside, so each is asked for again.
    ///
    /// It clears the in-flight note too: a request that went out before the column was
    /// explored would be answered with the same stale square, and keeping the note would
    /// let that answer back into the cache.
    fn evict(&mut self, column: MapColumn) {
        self.tiles.retain(|key, _| !key.contains(column));
        self.in_flight.retain(|key, _| !key.contains(column));
    }

    /// Forgets everything. The end of a session, and nothing else.
    fn clear(&mut self) {
        self.tiles.clear();
        self.in_flight.clear();
    }
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

/// Keeps the map's viewport and its tile cache in step with the server.
pub(super) struct MapUiPlugin;

impl Plugin for MapUiPlugin {
    fn build(&self, app: &mut App) {
        // Initialised here as well as by their producers, which is what keeps this module
        // headlessly testable on its own — the same reason every other panel does it.
        app.init_resource::<MapScreen>()
            .init_resource::<MapTiles>()
            .init_resource::<MapInbox>()
            .init_resource::<InputMode>()
            .init_resource::<PlayerStats>()
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
                    request_map_tiles,
                )
                    .chain(),
            );
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

/// Files every drawn square and applies every ledger page, in wire order.
fn ingest_map_payloads(mut inbox: ResMut<MapInbox>, mut tiles: ResMut<MapTiles>) {
    for event in inbox.take() {
        match event {
            MapEvent::Tile(tile) => tiles.insert(tile),
            MapEvent::Explored(explored) => {
                for column in explored.columns {
                    tiles.evict(column);
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
    tiles.in_flight.retain(|_, expires| *expires > now);

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
        tiles.in_flight.insert(key, now + REQUEST_RETRY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc::Receiver;

    use crate::net::{
        ANY_TOKEN, MAP_TILE_CELLS, MapExplored, MapSurface, SessionParams, map_tile_explored_bytes,
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
        app.add_plugins(MinimalPlugins)
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
        for expires in app
            .world_mut()
            .resource_mut::<MapTiles>()
            .in_flight
            .values_mut()
        {
            *expires = Duration::ZERO;
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

    #[test]
    fn a_closed_map_asks_for_nothing() {
        let (mut app, frames) = app();
        app.update();
        app.update();
        assert_eq!(requested(&frames), 0);
    }
}
