//! Input out, authoritative state in.
//!
//! This module is the client half of the rule the whole project is built on. It samples
//! the keyboard and the pointer, sends what the player is *trying* to do at the rate the
//! server asked for, and draws the answers that come back. It decides nothing:
//!
//! - **It never sends a position.** `PlayerInput` carries axes, a facing and a jump flag,
//!   and there is no field in the contract for anything else. So there is no claim for the
//!   server to validate, and no rejection path on either side to get wrong.
//! - **It runs no physics.** No gravity, no collision, no clamping of anything that
//!   matters. Walking into a wall stops the player because the *server* stopped them, and
//!   the client finds out when the next snapshot says so.
//! - **It does not predict.** Deliberately, and out of scope for this issue: on a local
//!   server the input latency is imperceptible, and prediction with reconciliation is a
//!   design of its own rather than something to smuggle into a skeleton. What the client
//!   does instead is draw one snapshot interval in the past and interpolate — see
//!   [`interpolate`], where the *"hold the last known position"* rule falls out of a clamp.
//!
//! - **It does not apply an edit either.** A click on a block sends a
//!   `BlockEditRequest` and waits; the voxel changes when the server's `BlockUpdate`
//!   arrives, and a refused edit therefore looks exactly like nothing happening. See
//!   [`target`].
//!
//! The one thing that *is* immediate is where the camera points, and that is not an
//! exception to the rule: `schemas/player.fbs` says the camera is a client concern. See
//! the module comment in [`camera`].
//!
//! ## Layout
//!
//! | Module | Owns |
//! | ------ | ---- |
//! | `mod.rs` | input sampling, the send cadence, the bodies the snapshots drive |
//! | `ambience.rs` | the cosmetic ground look read from loaded voxels around the eye |
//! | `birds.rs` | the ambient birds: the species table, the flight paths and the flap |
//! | `interpolate.rs` | the two-snapshot buffer and the interpolation — pure, no Bevy world |
//! | `drops.rs` | authoritative drop spawn/despawn and cosmetic cube motion |
//! | `hands.rs` | the camera-space held item and its cosmetic swing |
//! | `items.rs` | one row per item id: what it is called, its held shape, its colour |
//! | `inventory.rs` | the server-sent slots and the locally selected slot index |
//! | `crafting.rs` | the display-only recipe mirror and the craft intent it originates |
//! | `camera.rs` | the one camera, and what it follows |
//! | `sky.rs` | the sun, the sky colour, the ambient term and the fog, on the server's clock |
//! | `wards.rs` | the newest server-sent ward columns and their translucent boundary meshes |
//! | `target.rs` | the voxel raycast, mining intent/progress, placement and outline |
//! | `structures.rs` | the tents and forges a snapshot names, and the two requests for one |
//! | `constants.rs` | the numbers, and which of them mirror the server |
//! | `appearance.rs` | the rig: which box each appearance colour covers, and where it sits |

mod ambience;
mod appearance;
mod birds;
mod camera;
mod combat;
mod constants;
mod crafting;
mod drops;
mod hands;
mod interpolate;
mod inventory;
mod items;
mod livery;
mod loot;
mod mobs;
mod precipitation;
mod projectiles;
mod sky;
mod structures;
mod target;
mod vendor;
mod wards;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::ui::FocusPolicy;

pub(crate) use appearance::{
    ArmourPiece, ArmourSegment, BodyPart, BodyPiece, Limb, PlacedBox, envelope as body_envelope,
    held_item_anchor as body_held_item_anchor, held_item_box as body_held_item_box, piece_boxes,
    placed as placed_box, placed_armour,
};

// Issue #548 deliberately adds no production consumer, but the resource's public field
// must still expose a nameable type to the presentation modules that will consume it.
#[allow(unused_imports)]
pub use ambience::{Ambience, GroundLook};
pub(crate) use camera::{AimCamera, DeathFall};
pub use camera::{Orbit, ViewMode, WorldCamera};
// The character screen's preview is the same rig with no server entity behind it, so it
// is dressed out of the same wardrobe rather than from a second copy of the tables.
pub use crafting::{CraftClick, Ingredient, RECIPES, Recipe, RecipeCategory};
pub use interpolate::SnapshotBuffer;
#[cfg(test)]
pub(crate) use inventory::EQUIPMENT_ROUTES;
pub(crate) use inventory::equipment_item_fits;
pub use inventory::{
    ApplyInventory, Inventory, InventoryClick, InventoryClickKind, PickedStack, SelectedSlot,
};
pub use items::item_label;
#[cfg(test)]
pub(crate) use items::known_item_ids;
pub(crate) use items::{ITEM_SILVER, ItemShape, Livery, item_linear_rgba, item_livery, item_shape};
pub(crate) use livery::{Liveries, field_rect};
pub use loot::{LootTakeClick, LootWindow};
pub(crate) use sky::Daylight;
// The sky the low-health vignette has to be visible against. Test-only and deliberately
// so: `ui/health.rs` reads the colour to assert that its edge is not one the night has
// already reached (#553), and nothing at runtime may read a rule back out of it.
#[cfg(test)]
pub(crate) use sky::NIGHT_SKY;
pub use target::{ApplyMiningFeedback, HealTargetHint, MiningFeedback};
pub use vendor::{SHIFT_COUNT, VendorTradeClick, VendorWindow};

use crate::net::{
    Appearance, AppearanceInbox, BlockCoord, HairModel, LifeState, Outbound,
    PLACEHOLDER_APPEARANCE, PartyMemberState, PartyRosterMember, PlayerInput, PlayerVitals,
    ResidentInbox, ResidentRole, Sent, Session, SnapshotInbox, WeatherState, encode_player_input,
};
use crate::settings::{Bindings, Control, DEFAULT_LOOK_SENSITIVITY, Settings};
// The only edge from this file into the world module, and it is a read: a name plate asks
// what stands between the camera and a head, and tells the store nothing. `camera.rs` has
// the same one for the same reason, and `target.rs` says why the question lives there.
use crate::world::ChunkStore;
// `pub use` rather than `use` for the pitch limit: it is a build invariant rather than a
// preference, so it did not move to `crate::settings` with the sensitivity — and that
// module's test that no sensitivity this client offers can reach past it needs the limit
// itself. It stays in scope here, which is what keeps `sample_input` reading it by name.
use constants::DEATH_BODY_PITCH;
pub use constants::MAX_PITCH;

/// How far the player has to move before the movement log says so again, in blocks.
///
/// The log line exists because the debug overlay is on a screen, and a screen is what CI,
/// a remote session and an automated end-to-end check do not have — the same reason
/// `world/render.rs` logs when meshing settles. Keyed on distance rather than on time so a
/// player standing still is silent.
const MOVEMENT_LOG_DISTANCE: f32 = 8.0;

/// The hair model handed to [`piece_boxes`] for a piece that is not the hair, where it is
/// ignored.
///
/// Named rather than spelled at the call site, so nobody reads it as a default haircut:
/// `piece_boxes` is total over the piece, and every piece but [`BodyPiece::Hair`] has one
/// shape whatever is on the head above it.
const ANY_HAIR: HairModel = HairModel::Shaved;

/// How long an appearance for an entity nobody has drawn yet is kept.
///
/// **The bound on a cache the server fills.** An appearance legitimately arrives before
/// the snapshot that first carries its entity — `schemas/player.fbs` says either order is
/// legal and the server deliberately sends this one first — so an entry with no body must
/// survive a tick or two. It must not survive a session: nothing obliges a server to ever
/// send a snapshot naming an entity it described, and a client that kept every such
/// appearance would grow a map for as long as it stayed connected.
///
/// Two seconds is forty ticks at the default rate, which is two orders of magnitude more
/// than the gap it exists to cover and still a bound.
const APPEARANCE_GRACE: Duration = Duration::from_secs(2);

/// A fixed screen-space size keeps labels legible everywhere one is drawn at all.
const NAME_PLATE_WIDTH: f32 = 240.0;
const NAME_PLATE_HEIGHT: f32 = 28.0;
const NAME_PLATE_FONT_SIZE: f32 = 16.0;
const NAME_PLATE_GAP: f32 = 0.14;
/// Bound text layout on Unicode scalar boundaries without adding a grapheme crate.
const NAME_PLATE_CHARACTERS: usize = 48;

/// The furthest from the camera a name plate is drawn, in blocks.
///
/// **The number is chosen against the smallest world this client will ever draw, not
/// against the largest.** `crate::settings` will not take the render distance below two
/// chunks and a chunk is 32 blocks, so the nearest the fog can ever be brought in is 64
/// blocks; `ServerWelcome.view_distance` is a *ceiling* on that, and a server that streams
/// less simply has nothing further out to name. Half of that floor leaves the limit
/// comfortably inside the drawn world on every configuration there is, which is the
/// property that matters here: a plate must never be the thing that tells a player an
/// entity exists. The name arrives with the body or after it, never before it.
///
/// Thirty-two blocks is also about where a body stops being a person and becomes a
/// silhouette, so it is the distance at which a name stops labelling something the player
/// can see and starts tracking something they cannot.
const NAME_PLATE_DISTANCE: f32 = 32.0;

/// How far back inside [`NAME_PLATE_DISTANCE`] a hidden plate has to come before it is
/// drawn again, in blocks.
///
/// The distance half of the anti-flicker rule, and it is geometric rather than temporal
/// because the boundary is geometric: two players walking apart at the limit cross it
/// repeatedly on the noise in their interpolated positions alone, and a plate that answered
/// every crossing would strobe. Two blocks is wider than any one frame of that noise and
/// narrow enough that nobody can tell where the band is.
const NAME_PLATE_DISTANCE_MARGIN: f32 = 2.0;

/// How many consecutive frames a changed sight answer must hold before the plate follows
/// it.
///
/// The occlusion half of the anti-flicker rule, and this one has to be temporal because
/// occlusion has no band to widen: a fence post is either between the camera and the head
/// or it is not, and through a slow pan the answer alternates frame to frame. Requiring the
/// new answer four frames running turns that into nothing, and costs four frames of latency
/// on a plate that genuinely appears or disappears, comfortably under the tenth of a
/// second at which a player reads a change as a change rather than as a pop.
///
/// Symmetric on purpose. Appearing fast and fading slowly is the usual asymmetry and it is
/// the wrong one here, because the failure being prevented is a name flashing back on from
/// behind a wall.
const NAME_PLATE_SIGHT_DWELL: u8 = 4;

const DEFAULT_PLATE_COLOUR: Color = Color::WHITE;
pub(crate) const PARTY_PLATE_COLOUR: Color = Color::srgb(0.45, 0.82, 1.0);

/// The furthest a walking limb swings from rest.
///
/// A presentation angle only. Distance advances the phase in [`interpolate`], so this
/// number changes the silhouette and never how quickly anybody moves.
const WALK_SWING: f32 = 0.55;

/// Orders anything that reads the transforms a snapshot wrote after the systems that write
/// them.
///
/// Exported because [`camera`] has to run after this and a private system function cannot
/// be named from outside.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApplySnapshots;

/// Which family of controls currently owns the keyboard and pointer.
///
/// This is local presentation state, never a gameplay outcome. The server still owns
/// everything a playing input can cause; this resource only makes it impossible for a
/// camera turn or a block request to leak through an inventory or menu click.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Pointer captured, movement sampled, camera and block targeting live.
    #[default]
    Playing,
    /// Pointer captured and text entry owns the keyboard; gameplay input is closed.
    Chat,
    /// Pointer visible and confined over the authoritative inventory; horizontal movement
    /// remains live.
    Inventory,
    /// Pointer visible and confined over one authoritative corpse container.
    Loot,
    /// Pointer visible and confined over one authoritative stall.
    ///
    /// [`Self::Loot`]'s rules with a different window behind them, and deliberately not a
    /// second spelling of them: the pointer is visible because the whole content is rows,
    /// `Escape` closes it, the pack key is ignored rather than layering a second window over it,
    /// and death closes it because the server refuses every trade from a corpse anyway.
    Vendor,
    /// Pointer visible and confined while the pause menu is visible.
    Menu,
    /// Pointer visible and confined over the world map. Movement is closed, as it is for
    /// [`Self::Menu`] and unlike [`Self::Inventory`]: reading a map is not something a
    /// player does while walking, and a drag that panned the map would also be steering.
    Map,
}

/// Orders gameplay input after UI keys have chosen this frame's [`InputMode`].
///
/// The UI plugin owns the system in this set. Ordering against an empty set is a no-op,
/// which keeps `PlayerPlugin` usable by its headless tests without building any UI.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApplyInputMode;

/// Samples input, sends it, and draws what the server sends back.
pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        // Guarded, because `CharacterUiPlugin` builds it too and the two are independent —
        // Bevy panics on a unique plugin added twice.
        if !app.is_plugin_added::<BodyVisualsPlugin>() {
            app.add_plugins(BodyVisualsPlugin);
        }
        app.init_resource::<LookState>()
            .init_resource::<MoveIntent>()
            .init_resource::<InputMode>()
            .init_resource::<InputCadence>()
            .init_resource::<SnapshotBuffer>()
            .init_resource::<Appearances>()
            .init_resource::<SelfVitals>()
            .init_resource::<Party>()
            .init_resource::<PartyLogInbox>()
            .init_resource::<sky::SkyClock>()
            .init_resource::<Weather>()
            .init_resource::<Ambience>()
            .init_resource::<ambience::AmbienceState>()
            .init_resource::<PlayerStats>()
            // `init_resource` rather than `insert_resource`, and `NetPlugin` does the same:
            // whichever plugin is built first creates the inbox and the other finds it.
            .init_resource::<SnapshotInbox>()
            .init_resource::<AppearanceInbox>()
            .init_resource::<ResidentInbox>()
            .add_systems(
                Startup,
                (
                    drops::create_visuals,
                    mobs::create_visuals,
                    structures::create_visuals,
                    sky::spawn_sun,
                    sky::spawn_sky,
                    precipitation::create_visuals,
                    birds::create_visuals,
                ),
            )
            .add_systems(
                Update,
                (
                    // Sample, then send: an input frame must carry the controls as they
                    // are this frame, not as they were last one.
                    //
                    // And after the snapshot application set, because that is where this
                    // frame's authoritative vitals land: the frame the server says the
                    // player is dead has to be the frame their controls go quiet, not the
                    // one after it. Exactly why `sample_input` already runs after the UI
                    // system that chooses the mode — a gate read a frame late is a gate
                    // that leaks a frame of input.
                    (
                        sample_input.after(ApplyInputMode).after(ApplySnapshots),
                        send_player_input,
                    )
                        .chain(),
                    (
                        // Before both the party diff and the bodies, so a description and
                        // the snapshot that first names it in either surface share a name
                        // on that frame.
                        ingest_appearances,
                        ingest_snapshots,
                        apply_snapshots,
                        drops::apply_snapshots,
                        mobs::apply_snapshots,
                        structures::apply_snapshots,
                        sky::drive_the_sky,
                        // A body is spawned by a command, and a queued spawn is invisible
                        // to a query. Without this flush, the frame that first sees a
                        // player would leave the overlay reporting no position and the
                        // camera sitting at the spawn point — both read the transforms
                        // this set writes. It also makes a new drop's cosmetic child
                        // available to the animation system on that same frame.
                        ApplyDeferred,
                        // After the flush, because the children it writes to are spawned
                        // by a command and a queued spawn is invisible to a query.
                        dress_bodies,
                        // After the flush too: the local body is spawned by the same
                        // command, and this is what decides whether the player is looking
                        // at it. Inside the set rather than after it, so the visibility a
                        // frame draws is the one this frame's view asked for.
                        show_the_local_body,
                        // The phase came from the same interpolated snapshot positions
                        // `apply_snapshots` just wrote. Limbs compose below the body's
                        // root, before the death fall composes onto that root.
                        animate_walking_bodies,
                        pose_body_shields,
                        // **After `apply_snapshots`, which is a declared order and not an
                        // assumption.** That system writes the body's transform wholesale
                        // from the snapshot, and this composes a rotation onto it; the
                        // other order would have the snapshot overwrite the pose every
                        // frame and nothing would ever visibly fall over.
                        // **Inside this chain, after the snapshot and the body spawn it
                        // drives, and before the camera reads the local body's fall.** An
                        // independent system has already left the body a frame behind the
                        // camera on respawn once; the declared order is what makes every
                        // viewer see one answer on one frame.
                        collapse_bodies,
                        drops::animate,
                        mobs::animate,
                        structures::animate,
                        refresh_player_stats,
                    )
                        .chain()
                        .in_set(ApplySnapshots),
                    log_the_players_progress.after(ApplySnapshots),
                    forget_vitals_without_a_session.after(ApplySnapshots),
                    forget_weather_without_a_session.after(ApplySnapshots),
                    ambience::forget_ambience_without_a_session.after(ApplySnapshots),
                    forget_party_without_a_session.after(ApplySnapshots),
                    forget_snapshots_without_a_session.after(ApplySnapshots),
                    forget_bodies_without_a_session.after(ApplySnapshots),
                    // The selected slot can change in `ApplyInventory`, and the local body
                    // is materialised in `ApplySnapshots`. Read both answers only after
                    // their owners have published this frame's values.
                    refresh_body_held_item
                        .after(ApplySnapshots)
                        .after(ApplyInventory),
                )
                    .after(crate::net::DrainNetwork),
            )
            .add_plugins(projectiles::ProjectilesPlugin)
            .add_plugins(camera::PlayerCameraPlugin)
            // A look is read only after the camera owns this frame's eye, beside the
            // sky and the other presentation systems that consume no gameplay rule.
            .add_systems(Update, ambience::sample_the_ground.after(camera::AimCamera))
            // After the camera, because the flock is anchored to the eye, and after the
            // ground sample, because which species flies is the look this frame settled on.
            // The pair is chained: `keep_the_flock` decides what should exist and
            // `fly_the_flock` moves what does, and a frame between the two would draw a
            // newborn bird at the origin.
            .add_systems(
                Update,
                (birds::keep_the_flock, birds::fly_the_flock)
                    .chain()
                    .after(camera::AimCamera)
                    .after(ambience::sample_the_ground),
            )
            // After the camera plugin, because the walls follow the eye's coarse height
            // step and WardBoundaryPlugin orders its rebuild after AimCamera.
            .add_plugins(wards::WardBoundaryPlugin)
            .add_systems(
                Update,
                (
                    (sync_name_plates, position_name_plates).chain(),
                    mobs::face_aggro_markers,
                    // After the camera, because the volume is centred on the eye and its
                    // quads face it. Reading a frame early would turn fast-moving weather
                    // edge-on while the player turns.
                    precipitation::draw_precipitation,
                    // And after it for the same reason, one scale larger: the sky dome is
                    // centred on the eye, so a frame-old position slides the whole horizon
                    // rather than one quad. `sky::drive_the_sky` still runs inside
                    // `ApplySnapshots` and still takes the opposite trade for the colours.
                    sky::follow_the_eye,
                )
                    .after(camera::AimCamera),
            )
            .add_plugins(inventory::InventoryPlugin)
            // After the inventory plugin, because the craft gate is read against the
            // newest complete state and the ordering inside `CraftingPlugin` is written
            // against its system set.
            .add_plugins(crafting::CraftingPlugin)
            .add_plugins(loot::LootPlugin)
            .add_plugins(vendor::VendorPlugin)
            // After the camera plugin, because the ray starts at the camera and the
            // ordering inside `BlockTargetPlugin` is written against its system set.
            .add_plugins(combat::CombatPlugin)
            .add_plugins(target::BlockTargetPlugin)
            .add_plugins(structures::StructuresPlugin)
            .add_plugins(hands::HandsPlugin);
    }
}

// ---------------------------------------------------------------------------
// What the client knows
// ---------------------------------------------------------------------------

/// Where the player is looking, in radians.
///
/// The client's own state, and the only thing in this module that is not the server's
/// answer. It is sent as `PlayerInput.yaw` — which is how the server knows which way
/// "forward" points — and it is what the camera is rotated by.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct LookState {
    /// Facing, about the world's up axis. 0 looks along -Z.
    pub yaw: f32,
    /// Tilt, clamped to [`MAX_PITCH`]. Positive is up.
    pub pitch: f32,
}

/// What the movement controls are asking for this frame.
///
/// Raw intent in -1..=1 per axis, exactly as the contract describes it. The client does not
/// scale it by a speed, because it does not know one: how fast a player walks is the
/// server's number and never travels the other way.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct MoveIntent {
    /// Strafe, positive to the right of where the player is looking.
    pub x: f32,
    /// Forward, positive away from the player.
    pub z: f32,
    /// Whether the jump control is held. *Whether a jump happens* is the server's call.
    pub jump: bool,
}

/// Paces the input stream to the server's tick rate.
#[derive(Resource, Debug, Default)]
struct InputCadence {
    /// Elapsed time not yet spent on an input.
    credit: Duration,
    /// This client's own tick counter, sent for ordering and nothing else.
    client_tick: u32,
    sent: u32,
    dropped: u32,
}

impl InputCadence {
    /// Whether an input is due, given how long this frame took.
    ///
    /// The credit accumulates rather than resetting, so the average rate is the server's
    /// tick rate even when the frame time does not divide it: 40 ms frames against a 50 ms
    /// interval send on four frames out of five, where a reset would send on one in two.
    ///
    /// And the credit is capped at one interval, so a long stall — a window drag, a shader
    /// compile — becomes one input rather than a burst of identical ones. Same reasoning as
    /// the server's tick loop abandoning its missed ticks: every frame in such a burst
    /// describes the same controls, and only the newest is worth anything.
    fn due(&mut self, delta: Duration, interval: Duration) -> bool {
        self.credit += delta;
        if self.credit < interval {
            return false;
        }
        self.credit = (self.credit - interval).min(interval);
        true
    }
}

/// The meshes every body is drawn from, built once at startup and shared by everybody.
///
/// **Twenty-two meshes for a whole settlement.** Every player shares eleven fixed body
/// pieces, five hair meshes and six independently moving armour segments. Nothing here
/// is ever rebuilt: a description change swaps handles or optional children in place.
#[derive(Resource, Debug)]
pub(crate) struct PlayerVisuals {
    fixed: [(BodyPiece, Handle<Mesh>); BodyPiece::FIXED.len()],
    /// One per model, paired with the model it draws. An array rather than a map because
    /// there are five of them and `HairModel` is deliberately not a number.
    hair: [(HairModel, Handle<Mesh>); HairModel::ALL.len()],
    /// One shared overlay mesh per independently moving armour segment.
    armour: [(ArmourSegment, Handle<Mesh>); ArmourSegment::ALL.len()],
    shield: Handle<Mesh>,
}

impl PlayerVisuals {
    /// The mesh one independently moving piece wearing one hair model is drawn from.
    ///
    /// Total over [`BodyPiece`] with no wildcard arm, so a new piece does not compile
    /// until it has been routed to fixed geometry or the chosen hair model.
    fn mesh(&self, piece: BodyPiece, model: HairModel) -> Handle<Mesh> {
        match piece {
            BodyPiece::Hair => self
                .hair
                .iter()
                .find(|(drawn, _)| *drawn == model)
                .map_or_else(|| self.fixed[0].1.clone(), |(_, mesh)| mesh.clone()),
            BodyPiece::LeftShoe
            | BodyPiece::RightShoe
            | BodyPiece::LeftTrouser
            | BodyPiece::RightTrouser
            | BodyPiece::Torso
            | BodyPiece::LeftSleeve
            | BodyPiece::RightSleeve
            | BodyPiece::HeadAndNeck
            | BodyPiece::LeftFist
            | BodyPiece::RightFist
            | BodyPiece::Eyes => self
                .fixed
                .iter()
                .find(|(drawn, _)| *drawn == piece)
                .map_or_else(|| self.fixed[0].1.clone(), |(_, mesh)| mesh.clone()),
        }
    }

    fn armour_mesh(&self, segment: ArmourSegment) -> Handle<Mesh> {
        self.armour
            .iter()
            .find(|(drawn, _)| *drawn == segment)
            .map_or_else(|| self.armour[0].1.clone(), |(_, mesh)| mesh.clone())
    }
}

/// One material per colour and finish, rather than one per player.
///
/// Two people in the same walnut tunic share one `StandardMaterial`; two iron overlays
/// share another. Finish belongs in the key because identical colours with matte and
/// metallic surfaces are not the same material.
///
/// Swept by [`apply_snapshots`] rather than grown for ever: a server is free to describe a
/// colour nobody can choose, and sixteen million of them is a map. The sweep is triggered
/// by this map being larger than the cached appearances could justify rather than by the
/// cache changing size — see there for why the difference matters. An entry dropped while a
/// body still wears it costs nothing, because the body holds a strong handle to the
/// material and the next one asking for that colour simply makes it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BodyColour {
    /// A server-sent sRGB appearance colour.
    Srgb(u32),
    /// A display-registry colour, already converted to linear space.
    Linear([u32; 4]),
}

impl BodyColour {
    fn item(item_id: u16) -> Self {
        Self::Linear(item_linear_rgba(item_id).map(f32::to_bits))
    }

    fn colour(self) -> Color {
        match self {
            Self::Srgb(colour) => Color::srgb_u8(
                ((colour >> 16) & 0xFF) as u8,
                ((colour >> 8) & 0xFF) as u8,
                (colour & 0xFF) as u8,
            ),
            Self::Linear(channels) => {
                let [r, g, b, a] = channels.map(f32::from_bits);
                Color::linear_rgba(r, g, b, a)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BodyFinish {
    Matte,
    Leather,
    Iron,
}

impl BodyFinish {
    fn armour(item_id: u16) -> Self {
        match item_id {
            crafting::ITEM_LEATHER_CAP
            | crafting::ITEM_LEATHER_JERKIN
            | crafting::ITEM_LEATHER_LEGGINGS => Self::Leather,
            crafting::ITEM_IRON_HELM
            | crafting::ITEM_IRON_CUIRASS
            | crafting::ITEM_IRON_GREAVES => Self::Iron,
            // A newer server's item is still drawn in the registry's loud fallback
            // colour, but this build does not guess that it is metal.
            _ => Self::Matte,
        }
    }

    const fn roughness(self) -> f32 {
        match self {
            Self::Matte | Self::Leather => 0.9,
            Self::Iron => 0.55,
        }
    }

    const fn metallic(self) -> f32 {
        match self {
            Self::Matte | Self::Leather => 0.0,
            Self::Iron => 0.35,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct BodyMaterialKey {
    colour: BodyColour,
    finish: BodyFinish,
}

impl BodyMaterialKey {
    const fn appearance(colour: u32) -> Self {
        Self {
            colour: BodyColour::Srgb(colour),
            finish: BodyFinish::Matte,
        }
    }

    fn armour(item_id: u16) -> Self {
        Self {
            colour: BodyColour::item(item_id),
            finish: BodyFinish::armour(item_id),
        }
    }
}

#[derive(Resource, Debug, Default)]
pub(crate) struct BodyMaterials(HashMap<BodyMaterialKey, Handle<StandardMaterial>>);

impl BodyMaterials {
    /// The material for one colour, making it the first time it is asked for.
    fn of(
        &mut self,
        key: BodyMaterialKey,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.0
            .entry(key)
            .or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: key.colour.colour(),
                    perceptual_roughness: key.finish.roughness(),
                    metallic: key.finish.metallic(),
                    ..default()
                })
            })
            .clone()
    }
}

/// Everything dressing a body needs, as one borrow.
///
/// The three travel together — the shared meshes, the cache that keys a material on a
/// colour, and the assets that cache mints into — and both the system that spawns a body
/// and the system that re-dresses one need all three. Grouping them is what keeps
/// [`Self::outfit`] the single answer to "what is this character wearing", rather than two
/// loops that have to agree.
pub(crate) struct Wardrobe<'a> {
    visuals: &'a PlayerVisuals,
    palette: &'a mut BodyMaterials,
    materials: &'a mut Assets<StandardMaterial>,
}

impl Wardrobe<'_> {
    /// The mesh and material each part of one appearance is drawn with.
    pub(crate) fn outfit(
        &mut self,
        worn: Appearance,
    ) -> [(BodyPiece, Handle<Mesh>, Handle<StandardMaterial>); BodyPiece::ALL.len()] {
        let model = worn.hair_model();
        BodyPiece::ALL.map(|piece| {
            let part = piece.part();
            (
                piece,
                self.visuals.mesh(piece, model),
                self.palette.of(
                    BodyMaterialKey::appearance(part.colour(worn)),
                    self.materials,
                ),
            )
        })
    }

    /// The optional overlays named by the server's latest description.
    fn armour(
        &mut self,
        worn: Worn,
    ) -> Vec<(ArmourSegment, Handle<Mesh>, Handle<StandardMaterial>)> {
        ArmourSegment::ALL
            .into_iter()
            .filter_map(|segment| {
                let piece = segment.piece();
                let item_id = worn.armour(piece);
                (item_id != 0).then(|| {
                    (
                        segment,
                        self.visuals.armour_mesh(segment),
                        self.palette
                            .of(BodyMaterialKey::armour(item_id), self.materials),
                    )
                })
            })
            .collect()
    }

    fn shield(&mut self, worn: Worn) -> Option<(Handle<Mesh>, Handle<StandardMaterial>)> {
        (worn.off_hand == crafting::ITEM_WOODEN_SHIELD).then(|| {
            (
                self.visuals.shield.clone(),
                self.palette
                    .of(BodyMaterialKey::appearance(0x00ff_ffff), self.materials),
            )
        })
    }
}

/// The same three as one system parameter.
///
/// Grouped for the reason `net::Inboxes` is: two systems need all three, and a signature
/// that lists them is a signature nobody reads. [`Self::wardrobe`] is the only way in, so
/// the `Option` on the meshes is answered once rather than at each use.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct Dressing<'w> {
    visuals: Option<Res<'w, PlayerVisuals>>,
    palette: ResMut<'w, BodyMaterials>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
}

impl Dressing<'_> {
    /// Everything needed to dress a body, or `None` on a frame before the meshes exist.
    ///
    /// In practice there is no such frame — [`create_player_visuals`] runs in `Startup`
    /// and both readers run in `Update` — and it is an `Option` for the reason the
    /// resource always was: a client that took the window down because a plugin had not
    /// been built is a worse client than one that draws nothing for a frame.
    pub(crate) fn wardrobe(&mut self) -> Option<Wardrobe<'_>> {
        Some(Wardrobe {
            visuals: self.visuals.as_deref()?,
            palette: &mut self.palette,
            materials: &mut self.materials,
        })
    }
}

/// What every entity this session can see looks like.
///
/// **The cache the acceptance criterion asks for, and the reason it is here rather than
/// in `net`**: an entry may be dropped exactly when the body wearing it is despawned, and
/// this module is what despawns one. `net` knows when a message arrived; it does not know
/// when a player left.
///
/// Bounded in two directions. An entity that leaves both the view and the party takes its
/// entry with it. An entry that never finds a body or the party is dropped after
/// [`APPEARANCE_GRACE`]. Party membership is the one server-sent reason an out-of-view
/// description remains live: the HUD still has to name that row.
#[derive(Resource, Debug, Default)]
pub(crate) struct Appearances(HashMap<u64, Described>);

impl Appearances {
    pub(crate) fn identity(&self, entity_id: u64) -> Option<(String, u16)> {
        self.0.get(&entity_id).map(|described| {
            let level = match described.label {
                PlateLabel::Level(level) => level,
                // Unreachable: the party roster is the only caller and a party is made of
                // players. Zero rather than a second `Option` the caller would have to
                // invent a meaning for — a resident has no level.
                PlateLabel::Role(_) => 0,
            };
            (name_plate_name(&described.name), level)
        })
    }
}

/// The complete party answer carried by the newest accepted snapshot.
#[derive(Resource, Debug, Default, Clone, PartialEq)]
pub struct Party {
    /// Complete authoritative order, including this character and offline members.
    pub roster: Vec<PartyRosterMember>,
    /// Live combat values for the roster's other online members.
    pub members: Vec<PartyMemberState>,
}

/// Presentation lines derived from two accepted server answers, never from local intent.
#[derive(Resource, Debug, Default)]
pub(crate) struct PartyLogInbox(Vec<String>);

impl PartyLogInbox {
    pub(crate) fn take(&mut self) -> Vec<String> {
        std::mem::take(&mut self.0)
    }

    #[cfg(test)]
    pub(crate) fn push(&mut self, line: String) {
        self.0.push(line);
    }
}

/// What a name plate says beside the name — the one thing that differs between the two
/// kinds of body this module draws.
///
/// An enum rather than a level plus an optional role, because the pair has no fourth state:
/// a resident has no level and a player has no role, so a struct holding both would carry
/// two combinations nothing can produce and every reader would have to decide about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlateLabel {
    /// A player's server-derived level, guaranteed non-zero by the contract.
    Level(u16),
    /// A resident's trade, chosen by the settlement drawing that put them there.
    Role(ResidentRole),
}

/// The word a role is drawn as.
///
/// ASCII, and not by luck: `client/src/ui/mod.rs` fails this crate's build on a non-ASCII
/// literal, because the embedded fallback font is a 95-glyph subset. Total over
/// [`ResidentRole`], so a role the contract gains does not compile until it has a word.
const fn role_label(role: ResidentRole) -> &'static str {
    match role {
        ResidentRole::Villager => "Villager",
        ResidentRole::Smith => "Smith",
        ResidentRole::Carpenter => "Carpenter",
        ResidentRole::Cook => "Cook",
        ResidentRole::Trader => "Trader",
        ResidentRole::Guard => "Guard",
    }
}

/// One cached appearance: what the server said, when it said it, and whether anything
/// has been drawn wearing it yet.
///
/// **One cache for both kinds of body, deliberately.** A resident arrives on its own
/// message and is drawn from the same rig, dressed by the same system and labelled by the
/// same plate; a second map would be a second copy of every rule in [`apply_snapshots`]'s
/// retain, and a second place for a body to go undressed.
#[derive(Debug, Clone)]
struct Described {
    appearance: Appearance,
    name: String,
    label: PlateLabel,
    worn_head: u16,
    worn_chest: u16,
    worn_legs: u16,
    worn_offhand: u16,
    /// When this entry was written. Read only while `drawn` is false — once a body
    /// exists, that body's presence in the newest snapshot is what keeps the entry.
    at: Instant,
    /// Whether a body has ever been spawned wearing this. It is what separates *this
    /// entity has left* from *this entity has not arrived yet*, which are the same absence
    /// from a snapshot and want opposite answers.
    drawn: bool,
}

/// What the debug overlay reports about movement.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct PlayerStats {
    /// Where the server says this session's own player is. `None` until the first snapshot
    /// names it.
    pub position: Option<Vec3>,
    /// Its speed in blocks per second, from the velocity the snapshot carries.
    pub speed: Option<f32>,
    /// Entities in view, this player included.
    pub entities: usize,
    /// The newest server tick received.
    pub server_tick: Option<u32>,
    /// Input frames handed to the writer thread.
    pub inputs_sent: u32,
    /// Input frames dropped because the outbound queue was full.
    pub inputs_dropped: u32,
}

/// What the sky is doing where this player stands, as the newest **accepted** snapshot
/// left it.
///
/// A mirror and nothing more. `None` covers two states this side does not distinguish and
/// does not need to: no snapshot has arrived yet, and the server keeps no weather at all
/// (a test world and a pre-V26 server both legitimately do). Both draw a clear sky, which
/// is what a client that has been told nothing must show.
///
/// **Presentation only, and the decoder is where that is enforced.** `net/codec.rs`
/// already refuses a kind this build has no member for and a `Clear` carrying a non-zero
/// intensity, so an out-of-contract value ends the session rather than reaching here.
/// Nothing downstream reads a *rule* back out of these two bytes: the cold, the slowed
/// step and the doused fire are the server's, and they arrive as vitals, as position and
/// as `StructureState::lit`.
///
/// It rides the same acceptance gate [`SelfVitals`] does — a snapshot the buffer refused
/// describes a moment already drawn, and letting it set the weather would flicker the sky
/// back to a tick that has been and gone.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Weather(Option<WeatherState>);

impl Weather {
    /// What the server last said the sky was doing, if it has said anything.
    pub(crate) fn get(self) -> Option<WeatherState> {
        self.0
    }
}

/// The vitals the newest accepted snapshot carried, or `None` before one has arrived.
///
/// Replaced wholesale, exactly as [`Inventory`] is. `self_vitals` is present in every
/// snapshot by contract, so there is nothing to merge and nothing to advance: health and
/// hunger are never incremented or drained here, damage is never applied here, and the
/// respawn count is never run down from local time. A dropped snapshot costs nothing,
/// because the next one carries the complete answer.
///
/// `None` is the honest encoding of *the server has not said yet*, and it is what the end
/// of a session restores. It deliberately does **not** read as dead — see [`Self::dead`].
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct SelfVitals(Option<PlayerVitals>);

impl SelfVitals {
    /// What the server last said about this player, if it has said anything.
    pub fn get(&self) -> Option<PlayerVitals> {
        self.0
    }

    /// Whether the server currently says this player is dead.
    ///
    /// **This is the one shared input gate**, read by movement sampling, aiming, mining,
    /// placement, hotbar selection, inventory moves and the inventory toggle. It is
    /// presentation and bandwidth hygiene rather than authority: the server refuses a
    /// forged request from a dead player whatever this answers, and it is the server that
    /// decides when they stop being dead.
    ///
    /// Absent vitals read as *not dead*, which is the direction that fails safe here. A
    /// client that had heard nothing yet would otherwise lock a perfectly alive player out
    /// of their own controls, and that is a gameplay decision made locally — the one thing
    /// this module may never do.
    pub fn dead(&self) -> bool {
        matches!(self.0, Some(vitals) if vitals.life_state == LifeState::Dead)
    }

    /// Server-sent vitals without a socket, so the UI that draws them can be exercised
    /// headlessly. Test-only, for the reason `SnapshotInbox::push` is: this is the one
    /// shape a system must not be able to construct, because a client that could name its
    /// own health would be inventing the number it is meant to be told.
    #[cfg(test)]
    pub(crate) fn from_server(vitals: PlayerVitals) -> Self {
        Self(Some(vitals))
    }
}

/// The gate every playing control is read through.
///
/// One `SystemParam` rather than the same pair of resources threaded through six
/// signatures, so that *may this frame's input act on the world?* has one answer and one
/// place to change it. Three questions live here because they are genuinely different, and
/// each system asks the one it means:
///
/// - [`Self::may_move`] — may the horizontal movement axes reach the server. Continuous
///   intent, and deliberately still live while the inventory is open.
/// - [`Self::may_aim`] — may the crosshair resolve a voxel. A continuous query, so the
///   frame a mode changes on is allowed to produce an outline.
/// - [`Self::may_act`] — may a request leave this client. Edge-triggered, so the frame a
///   mode changes on belongs to the UI and produces nothing.
///
/// **None is authority.** The server owns every outcome an input could ask for and
/// refuses a forged one whatever this answers. What the gate buys is usability and
/// bandwidth: a dead player's controls go quiet instead of firing requests into a
/// refusal, and the client never has to guess which of them the server would have taken.
///
/// The fields stay private, so `ui/` reads the gate through these methods and only
/// this module and its children — where the bespoke conditions live — reach past them.
#[derive(SystemParam)]
pub struct InputGate<'w> {
    mode: Res<'w, InputMode>,
    vitals: Res<'w, SelfVitals>,
    view: Res<'w, ViewMode>,
}

impl InputGate<'_> {
    /// Which family of controls owns the keyboard and pointer this frame.
    pub fn mode(&self) -> InputMode {
        *self.mode
    }

    /// Whether the server currently says this player is dead. See [`SelfVitals::dead`].
    pub fn dead(&self) -> bool {
        self.vitals.dead()
    }

    /// Which view the world is being drawn in. See [`ViewMode`].
    pub fn view(&self) -> ViewMode {
        *self.view
    }

    /// Whether the horizontal movement axes may reach the server.
    ///
    /// Inventory keeps walking live so opening the pack does not root the character. The
    /// pointer, jump, targeting and world actions remain closed through their own gates;
    /// chat, loot and the pause menu still own movement as well as their other input.
    pub fn may_move(&self) -> bool {
        matches!(*self.mode, InputMode::Playing | InputMode::Inventory) && !self.vitals.dead()
    }

    /// Whether aiming, targeting and the outline are live.
    pub fn may_aim(&self) -> bool {
        *self.mode == InputMode::Playing && !self.vitals.dead() && self.view.first_person()
    }

    /// Whether a gameplay request may be originated this frame.
    ///
    /// Stricter than [`Self::may_aim`] by the mode's change flag: a transition and the key
    /// or click that caused it share a frame, and treating that frame as UI-owned is what
    /// keeps clicking *Resume* from also swinging at the block behind the button.
    ///
    /// **The view term is repeated rather than inherited, and that is the point.** These
    /// are two independent expressions over the same inputs — `may_act` is not defined as
    /// `may_aim` plus a condition — so a term added to one is simply absent from the
    /// other. Closing only `may_aim` for third person would hide the crosshair and the
    /// outline and leave every request still reachable: no sight, and clicking still
    /// mines. Third person closes both.
    pub fn may_act(&self) -> bool {
        *self.mode == InputMode::Playing
            && !self.mode.is_changed()
            && !self.vitals.dead()
            && self.view.first_person()
    }
}

/// The shared part meshes and the material-per-colour cache a dressed body is built from.
///
/// Its own plugin because two things dress bodies now: the world, and the character
/// screen's turning preview. `CharacterUiPlugin` builds it too, which is what keeps that
/// screen headlessly testable on its own — the same reason every other panel initialises
/// the resources its systems read.
///
/// Both callers guard with `is_plugin_added`, because Bevy panics on a unique plugin added
/// twice and the two are built independently.
pub(crate) struct BodyVisualsPlugin;

impl Plugin for BodyVisualsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BodyMaterials>()
            .add_systems(Startup, create_player_visuals);
    }
}

/// Marks an entity the snapshots drive, and carries the identity it is drawn for.
#[derive(Component, Debug, Clone, Copy)]
struct Body(u64);

/// One screen-space label tied to the authoritative entity id, never to its text.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct NamePlate(u64);

/// Whether one plate's owner is somewhere the player could see it, and how long the raw
/// answer has been disagreeing.
///
/// Presentation state and nothing else. It decides what this client draws over its own
/// view; the server has already decided which entities this session is told about, and
/// nothing here is sent anywhere or read back as a rule.
///
/// `shown` is the settled answer, `dwell` is how many consecutive frames the unsettled one
/// has differed from it. Keeping the counter beside the answer rather than in a resource is
/// what makes the two rules per-plate: a name plate strobing behind a fence post must not be
/// steadied or disturbed by what any other plate is doing.
///
/// `near` is the distance rule's *own* settled answer, and it is a separate field rather
/// than a read of `shown` because the two rules are only independent if their hysteresis is
/// too. Choosing the threshold from `shown` couples them in the direction that is least
/// obvious and hurts most: a plate hidden by a wall at 31 blocks would come back needing the
/// *hidden* threshold of 30, so clearing the wall without also walking a block closer left
/// the plate off with a clear line of sight and the distance rule plainly satisfied. The
/// band belongs to the distance rule, so it is judged against the distance rule's own
/// history and against nothing else.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PlateSight {
    shown: bool,
    dwell: u8,
    near: bool,
}

/// Marks the body belonging to this session. Exactly one entity ever has it.
#[derive(Component)]
pub struct LocalPlayer;

/// What a drawn body is currently wearing.
///
/// Kept on the entity rather than read back out of its children's materials, for the
/// reason a mob keeps its interpolated yaw: recovering it would mean asking six handles
/// what colour they are and reversing the map that made them. It is also what makes
/// dressing a body idempotent — an appearance that has not changed changes nothing.
///
/// The local player carries the same component and overlays as everybody else; first
/// person hides the body rather than constructing a second wardrobe.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct Worn {
    appearance: Appearance,
    head: u16,
    chest: u16,
    legs: u16,
    off_hand: u16,
}

impl Worn {
    const fn bare(appearance: Appearance) -> Self {
        Self {
            appearance,
            head: 0,
            chest: 0,
            legs: 0,
            off_hand: 0,
        }
    }

    fn described(description: &Described) -> Self {
        Self {
            appearance: description.appearance,
            head: description.worn_head,
            chest: description.worn_chest,
            legs: description.worn_legs,
            off_hand: description.worn_offhand,
        }
    }

    const fn armour(self, piece: ArmourPiece) -> u16 {
        match piece {
            ArmourPiece::Head => self.head,
            ArmourPiece::Chest => self.chest,
            ArmourPiece::Legs => self.legs,
        }
    }

    fn material_keys(self) -> impl Iterator<Item = BodyMaterialKey> {
        ArmourPiece::ALL
            .into_iter()
            .map(move |piece| self.armour(piece))
            .filter(|item_id| *item_id != 0)
            .map(BodyMaterialKey::armour)
            .chain(
                (self.off_hand == crafting::ITEM_WOODEN_SHIELD)
                    .then_some(BodyMaterialKey::appearance(0x00ff_ffff)),
            )
    }
}

/// One independently placeable mesh of one body.
#[derive(Component, Debug, Clone, Copy)]
struct BodyVisual(BodyPiece);

/// One optional equipment segment, kept separate from the base rig so the character
/// preview remains bare and a server update can add or remove it in place.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct ArmourVisual(ArmourSegment);

/// A snapshot-driven left-forearm shield.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct ShieldVisual;

/// The stride sample the interpolator produced for this frame.
///
/// Stored beside the root transform so the child-animation system consumes the exact
/// sample that placed the body. It is presentation state only and never leaves the ECS.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
struct WalkPose {
    phase: f32,
    moving: bool,
}

impl From<&interpolate::Interpolated> for WalkPose {
    fn from(state: &interpolate::Interpolated) -> Self {
        Self {
            phase: state.walk_phase,
            moving: state.walking,
        }
    }
}

/// The second piece of a held item, when its material is not the item's.
///
/// A sword's grip, today. It hangs under the item rather than beside it so the fist's
/// transform, the arm swing and the visibility all reach it for free — and so a despawn takes
/// it with the thing it belongs to.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct BodyHeldPiece;

/// The item drawn in this session's body hand.
///
/// It exists only for a non-empty authoritative selected stack. The selected index is
/// local input, but the item id comes from [`Inventory`], which is replaced only by a
/// complete state from the server.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct BodyHeldItem {
    item_id: u16,
    shape: ItemShape,
}

/// Which one of the two held-item renderers owns the current view.
///
/// One answer is read by both renderers, so a view or pause condition cannot be added to
/// one and forgotten by the other. `Hidden` is the honest answer outside a live playing
/// session: neither a camera-space model nor its world-space mirror is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldItemSurface {
    Hidden,
    ViewModel,
    Body,
}

fn held_item_surface(mode: InputMode, view: ViewMode, session_exists: bool) -> HeldItemSurface {
    if !matches!(mode, InputMode::Playing | InputMode::Chat) || !session_exists {
        HeldItemSurface::Hidden
    } else if view.first_person() {
        HeldItemSurface::ViewModel
    } else {
        HeldItemSurface::Body
    }
}

/// The item id in a non-empty stack, or the empty-hand answer.
///
/// Shared by the first-person composition and the body mirror so zero-count and zero-id
/// stacks cannot become two different pictures in the two views.
fn stack_item_id(stack: Option<crate::net::InventoryStack>) -> Option<u16> {
    stack
        .filter(|stack| stack.item_id != 0 && stack.count != 0)
        .map(|stack| stack.item_id)
}

/// The local attachment of one world-scale item to the right fist.
///
/// Ordinary shapes retain the centre placement they already had. A blade is carried
/// **pointing where the character looks**, standing, walking and turning alike: a drawn
/// weapon reaches forward, and nothing here is conditional on movement, aim or combat.
///
/// **Forward is `-Z`, and that is the one sign this whole placement rests on.**
/// `appearance::placed_in_layer` states it where the model sheet is read — the sheet
/// measures forwards as `+z` and a body faces `-Z`, one negation in one place — and the body
/// entity's own rotation is `Quat::from_rotation_y(state.yaw)`, so the rig's local `-Z` is the
/// facing direction at every yaw. Get it backwards and the sword points out of the
/// character's back, which is why
/// [`the_local_body_holds_the_authoritative_selected_item_at_world_scale`] pins both axes of
/// the rotation rather than the pose it produces.
///
/// **One quarter turn about X does it, and both of its axis mappings are load-bearing.** The
/// sword mesh is built tip-up: local `+Y` is the tip and local `+Z` spans the cross guard,
/// which is also the blade's width axis. `from_rotation_x(-FRAC_PI_2)` sends local `+Y` to
/// `-Z` (the tip reaches forward) and local `+Z` to `+Y` (the guard stands upright). An
/// upright guard is what an ordinary forward grip looks like — the blade's flat is vertical
/// and its two edges face up and down — and from a camera behind the character it reads as a
/// crossbar instead of as a point.
///
/// **Seated by the cross guard, not by the grip.** The fist is 0.20 blocks through and the
/// grip only 0.082, so a grip-centred seating leaves the guard inside the box. Putting the
/// guard's rearward face — [`drops::blade_guard_base`], the face the grip enters — on the
/// fist's forward face instead closes exactly the grip and the pommel inside the fist, sits
/// the guard immediately in front of it, and sends the whole blade forward clear of the body.
///
/// **No outboard offset, and its absence is the measurement rather than an omission.** The
/// hanging pose this replaces needed one: a yawed, hanging guard swept about 0.076 blocks
/// either side of the hang axis and would have buried its inboard tip in the tunic hem, which
/// reaches `x = 0.25`. An upright guard spends its length on `Y` and is only `GUARD_SIZE.x`
/// thick across the body, so seated on the fist's centre at `x = 0.30` the sword's widest
/// point still clears that hem. The test measures the clearance; nothing here tunes it.
///
/// **The honest cost of this pose**: the rig's right arm hangs at the side and swings with
/// the walk cycle, and there is no arm pose that raises it, so a sword reaching forward out of
/// a hanging fist reads as a wrist bent ninety degrees. Pointing forward is the requested
/// behaviour and this is what it costs; raising the arm is a separate change that would have
/// to fight the walk animation.
fn body_held_item_transform(shape: ItemShape) -> Transform {
    let anchor = body_held_item_anchor() - BodyPiece::RightFist.pivot();
    if shape != ItemShape::Blade {
        return Transform::from_translation(anchor);
    }
    let fist = body_held_item_box();

    use std::f32::consts::FRAC_PI_2;

    // Tip to -Z, guard span to +Y. A quarter turn is an exact signed permutation of the
    // axes, so every axis-aligned part of the model sheet keeps an axis-aligned box here
    // rather than acquiring an inflated one — which is what lets the test separate the
    // sword from the rig one axis at a time.
    let rotation = Quat::from_rotation_x(-FRAC_PI_2);
    // The shared drop mesh is deliberately small on the ground. A modest body-only scale
    // makes its furniture readable beside the 1.8-block rig without changing that asset.
    const BODY_BLADE_SCALE: f32 = 1.25;
    let scale = Vec3::splat(BODY_BLADE_SCALE);
    // Where the guard's rearward face has to land: on the fist's forward face, which is its
    // *lowest* z, because the body faces -Z.
    let guard_seat = anchor - Vec3::Z * (fist.size.z / 2.0);
    Transform {
        translation: guard_seat - rotation * (drops::blade_guard_base() * scale),
        rotation,
        scale,
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

fn create_player_visuals(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(PlayerVisuals {
        fixed: BodyPiece::FIXED.map(|piece| (piece, meshes.add(piece_mesh(piece, ANY_HAIR)))),
        hair: HairModel::ALL.map(|model| (model, meshes.add(piece_mesh(BodyPiece::Hair, model)))),
        armour: ArmourSegment::ALL
            .map(|segment| (segment, meshes.add(armour_segment_mesh(segment)))),
        shield: meshes.add(hands::shield_mesh(0.62)),
    });
}

/// One independently placeable piece of the rig, merged into a single mesh.
///
/// **Authored with its origin at the feet**, which is where the server puts the position
/// it sends, exactly as a mob's parts are. So a body entity's `Transform` is the *feet*
/// position the snapshot carries rather than a centre only this module would know about,
/// the children carry no offset of their own, and the camera can add an eye height to the
/// same number. The capsule this replaces baked the same property in by translating
/// itself up half a body; `player::appearance` measures from the ground instead, so
/// nothing here has to.
///
/// Merged per piece rather than per box. An arm needs a sleeve and fist because they wear
/// different colours, while the head and neck share a transform and stay one mesh.
fn piece_mesh(piece: BodyPiece, model: HairModel) -> Mesh {
    let part = piece.part();
    let pivot = piece.pivot();
    let mut boxes = piece_boxes(piece, model).iter().map(|cell| {
        let placed = placed_box(part, *cell);
        Mesh::from(Cuboid::from_size(placed.size)).translated_by(placed.centre - pivot)
    });

    // Unreachable: every part in the table is drawn from at least one box, and
    // `every_hair_model_is_a_silhouette_of_its_own` is what says so. An empty mesh is the
    // cosmetic failure the rest of this module already prefers to a panic in a renderer.
    let Some(mut merged) = boxes.next() else {
        error!("{piece:?} is drawn from no boxes at all");
        return Mesh::from(Cuboid::from_size(Vec3::ZERO));
    };
    merge_all(&mut merged, boxes, "player body");
    merged
}

/// One worn segment authored around the pivot of the body piece underneath it.
///
/// A cuirass is one logical server slot but its sleeves move with the arms, just as the
/// two greaves move with their respective legs. The six shared meshes preserve those
/// pivots without changing the three-slot wire contract.
fn armour_segment_mesh(segment: ArmourSegment) -> Mesh {
    let placed = placed_armour(segment.piece(), segment.cell());
    Mesh::from(Cuboid::from_size(placed.size))
        .translated_by(placed.centre - segment.body_piece().pivot())
}

/// Reads the controls into [`MoveIntent`] and [`LookState`].
///
/// Both input resources are optional, so this module works in an app built without
/// `InputPlugin` — which is every one of its own tests. Absent input is no input, not a
/// panic: `Res<T>` on a missing resource takes the whole app down, and a client that exits
/// because it has no keyboard is a worse client than one that stands still.
fn sample_input(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    pointer: Option<Res<AccumulatedMouseMotion>>,
    gate: InputGate<'_>,
    settings: Option<Res<Settings>>,
    mut intent: ResMut<MoveIntent>,
    mut look: ResMut<LookState>,
    mut orbit: ResMut<Orbit>,
) {
    // What the player asked for, or what this client ships with. Optional for the reason
    // the two input resources above are: every one of this module's own tests builds an
    // app without the settings plugin, and a `Res<T>` on a missing resource takes the
    // whole app down. The defaults are what those tests are written against.
    let (sensitivity, bindings) = match settings.as_deref() {
        Some(settings) => (settings.look_sensitivity(), *settings.bindings()),
        None => (DEFAULT_LOOK_SENSITIVITY, Bindings::default()),
    };

    // Held, not pressed: PlayerInput describes the state of the controls each tick, so what
    // matters is whether the key is down when the frame is sampled.
    let horizontal_axes = |keys: &ButtonInput<KeyCode>| {
        let axis = |negative: KeyCode, positive: KeyCode| {
            f32::from(keys.pressed(positive)) - f32::from(keys.pressed(negative))
        };
        (
            axis(bindings.key(Control::Left), bindings.key(Control::Right)),
            axis(bindings.key(Control::Back), bindings.key(Control::Forward)),
        )
    };

    // A mode transition and its key or pointer event share a frame. Treat world-facing
    // input as UI-owned, so clicking Resume cannot also swing at the block behind the
    // button. Inventory is the one deliberate split: its horizontal movement is continuous
    // intent, while jump, pointer input and actions remain closed.
    if *gate.mode != InputMode::Playing || gate.mode.is_changed() {
        let next = if gate.may_move() {
            keys.as_deref().map_or_else(MoveIntent::default, |keys| {
                let (x, z) = horizontal_axes(keys);
                MoveIntent { x, z, jump: false }
            })
        } else {
            MoveIntent::default()
        };
        set_if_changed(&mut intent, next);
        // Released rather than left as it was: a player who opened UI with the orbit key
        // down is not holding it any more as far as this client is concerned, and an orbit
        // that never settles would leave the camera off to one side for the rest of the
        // session.
        if orbit.held {
            orbit.held = false;
        }
        return;
    }

    // **Which angle the mouse moves, and the only place that is decided.** Third person
    // with the orbit key held moves a camera-only offset; everything else moves the
    // character's own look. That is what keeps holding the key from spinning the player on
    // the server — `LookState::yaw` is what `PlayerInput` carries, and the orbit is not
    // in it.
    let orbiting = !gate.view().first_person()
        && keys.as_ref().is_some_and(|keys| {
            keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
        });
    if orbit.held != orbiting {
        orbit.held = orbiting;
    }

    if let Some(pointer) = pointer
        && pointer.delta != Vec2::ZERO
    {
        // Right turns right: looking along -Z, turning towards +X is a *negative* rotation
        // about +Y. Screen y grows downward, so a downward drag has to lower the pitch.
        let yaw = -pointer.delta.x * sensitivity;
        let pitch = -pointer.delta.y * sensitivity;
        if orbiting {
            // Unclamped here; `camera_placement` clamps the sum, so a swung camera and a
            // raised head cannot add up to more pitch than either could reach alone.
            let next = Orbit {
                yaw: orbit.yaw + yaw,
                pitch: orbit.pitch + pitch,
                held: orbit.held,
            };
            set_if_changed(&mut orbit, next);
        } else {
            let next = LookState {
                yaw: look.yaw + yaw,
                pitch: (look.pitch + pitch).clamp(-MAX_PITCH, MAX_PITCH),
            };
            // Wrapped here rather than left to grow, so the yaw stays a number a lerp can
            // use: the server wraps what it echoes, and a client whose own copy had
            // drifted a thousand turns away would disagree with every snapshot about which
            // way it faces.
            set_if_changed(&mut look, wrap_look(next));
        }
    }

    // Dead, as the *server* says. The axes are zeroed rather than the input stream being
    // stopped: `PlayerInput` still has to carry the yaw, and a client that went quiet
    // because it had decided something about its own state would be deciding. This is
    // usability and bandwidth — the server refuses a dead player's movement either way.
    //
    // Below the look block on purpose. Where the camera points is a client concern that
    // `schemas/player.fbs` names as one, and a corpse that cannot look around is a bug
    // rather than a rule.
    if gate.dead() {
        set_if_changed(&mut intent, MoveIntent::default());
        return;
    }

    let Some(keys) = keys else {
        return;
    };

    let (x, z) = horizontal_axes(&keys);
    let next = MoveIntent {
        x,
        z,
        jump: keys.pressed(bindings.key(Control::Jump)),
    };
    // Opposite keys cancel, and both axes are left un-normalised on purpose: the diagonal
    // (1, 1) is a vector of length √2, and scaling it is the *server's* speed clamp to
    // apply. A client that normalised here would be doing the server's job, and a client
    // that did not would be no faster for it.
    set_if_changed(&mut intent, next);
}

/// Sends one tick of intent, at the rate the server announced.
///
/// Tick-driven rather than frame-driven, which is what the contract asks for: the client
/// samples the latest control state and emits at `ServerWelcome.tick_rate`, so a 240 Hz
/// machine does not send twelve times as much input as a 20 Hz one.
fn send_player_input(
    time: Res<Time>,
    session: Option<Res<Session>>,
    outbound: Option<ResMut<Outbound>>,
    look: Res<LookState>,
    intent: Res<MoveIntent>,
    mut cadence: ResMut<InputCadence>,
) {
    // No session means the server has not said what rate to send at; no outbound means
    // there is no thread left to send through. Either way there is nothing to do, and the
    // cadence is left untouched so it does not accumulate credit for a session that
    // does not exist.
    let (Some(session), Some(mut outbound)) = (session, outbound) else {
        return;
    };

    if !cadence.due(time.delta(), tick_interval(session.0.tick_rate)) {
        return;
    }

    cadence.client_tick = cadence.client_tick.wrapping_add(1);
    let frame = encode_player_input(&PlayerInput {
        client_tick: cadence.client_tick,
        move_x: intent.x,
        move_z: intent.z,
        yaw: look.yaw,
        pitch: look.pitch,
        jump: intent.jump,
    });

    match outbound.send(frame) {
        Sent::Queued => cadence.sent += 1,
        // Counted, not logged. A full queue means the socket is behind, which is worth
        // seeing on the overlay and is not worth a line twenty times a second.
        Sent::Dropped => cadence.dropped += 1,
        // The session has ended and `drain_session_events` has not caught up yet. It will
        // remove the resource this frame or the next, and this stops being reachable.
        Sent::Closed => {}
    }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// Moves the snapshots the net thread decoded into the buffer that interpolates them.
///
/// Ordered after the drain, so a snapshot that arrived this frame is drawn this frame.
fn ingest_snapshots(
    mut inbox: ResMut<SnapshotInbox>,
    mut buffer: ResMut<SnapshotBuffer>,
    mut outputs: SnapshotOutputs<'_>,
) {
    let arrived = inbox.take();
    if arrived.is_empty() {
        // Guarded, because `ResMut` marks a resource changed on every `DerefMut`: taking an
        // empty inbox every frame would leave it permanently "changed".
        return;
    }

    for (snapshot, at) in arrived {
        // Copied out before the buffer takes ownership, and published only for a snapshot
        // the buffer *accepted*: a duplicate or out-of-order tick carries the vitals of a
        // moment already drawn, and applying them would walk health backwards.
        let self_vitals = snapshot.self_vitals;
        // The time of day rides the same gate for the same reason. A frame that arrived
        // late describes a moment already drawn, and letting it anchor the sky would run
        // the sun backwards — the one thing `SkyClock` promises it never does.
        let tick_of_day = snapshot.tick_of_day;
        // And the weather, on the same gate for the same reason: a late frame describes a
        // sky that has already been drawn.
        let weather = snapshot.weather;
        let next_party = Party {
            roster: snapshot.party_roster.clone(),
            members: snapshot.party_members.clone(),
        };
        if buffer.accept(snapshot, at) {
            // The whole value, never a merge. `set_if_changed` because an unchanged answer
            // is not news — it is what lets the death countdown hold rather than churn the
            // UI that reads it.
            set_if_changed(&mut outputs.vitals, SelfVitals(Some(self_vitals)));
            outputs.sky.anchor(tick_of_day, at);
            set_if_changed(&mut outputs.weather, Weather(weather));
            let recipient = outputs
                .session
                .as_deref()
                .map(|session| session.0.entity_id);
            let old_ids: HashSet<u64> = outputs
                .party
                .roster
                .iter()
                .filter(|member| Some(member.entity_id) != recipient)
                .map(|member| member.character_id)
                .collect();
            let new_ids: HashSet<u64> = next_party
                .roster
                .iter()
                .filter(|member| Some(member.entity_id) != recipient)
                .map(|member| member.character_id)
                .collect();
            for character_id in new_ids.difference(&old_ids) {
                let name = next_party
                    .roster
                    .iter()
                    .find(|member| member.character_id == *character_id)
                    .map_or("Unknown", |member| member.name.as_str());
                outputs.party_log.0.push(format!("{name} joined the party"));
            }
            for character_id in old_ids.difference(&new_ids) {
                let name = outputs
                    .party
                    .roster
                    .iter()
                    .find(|member| member.character_id == *character_id)
                    .map_or_else(|| "Unknown".to_owned(), |member| member.name.clone());
                outputs.party_log.0.push(format!("{name} left the party"));
            }
            if let Some(session) = outputs.session.as_deref()
                && next_party.roster.first().map(|leader| leader.entity_id)
                    == Some(session.0.entity_id)
                && outputs.party.roster.first().map(|leader| leader.entity_id)
                    != Some(session.0.entity_id)
            {
                outputs
                    .party_log
                    .0
                    .push("You are now the party leader".to_owned());
            }
            set_if_changed(&mut outputs.party, next_party);
        } else {
            // Server ticks are monotonic per session, so this is a duplicate. Debug rather
            // than warn: it costs nothing and means nothing went wrong.
            debug!("a snapshot that was not newer than the newest held was discarded");
        }
    }
}

#[derive(SystemParam)]
struct SnapshotOutputs<'w> {
    vitals: ResMut<'w, SelfVitals>,
    sky: ResMut<'w, sky::SkyClock>,
    weather: ResMut<'w, Weather>,
    session: Option<Res<'w, Session>>,
    party: ResMut<'w, Party>,
    party_log: ResMut<'w, PartyLogInbox>,
}

fn forget_party_without_a_session(
    session: Option<Res<Session>>,
    mut party: ResMut<Party>,
    mut party_log: ResMut<PartyLogInbox>,
) {
    if session.is_none() {
        set_if_changed(&mut party, Party::default());
        if !party_log.0.is_empty() {
            party_log.0.clear();
        }
    }
}

/// Ensures a reconnect accepts its own tick sequence and reconstructs only from it.
fn forget_snapshots_without_a_session(
    session: Option<Res<Session>>,
    mut buffer: ResMut<SnapshotBuffer>,
) {
    if session.is_none() && !buffer.is_empty() {
        buffer.clear();
    }
}

/// Forgets the server's vitals once there is no session to have had them.
///
/// The combat HUD hides on the condition every other permanent panel hides on — no
/// `Session`, nothing drawn — and this is the resource half of the same rule: health from a
/// session that has ended is not health, and leaving it behind would have a later one
/// inherit it. Idempotent, because `ResMut` marks a resource changed on every `DerefMut`
/// and this runs on every frame for the rest of the app's life.
fn forget_vitals_without_a_session(session: Option<Res<Session>>, mut vitals: ResMut<SelfVitals>) {
    if session.is_none() {
        set_if_changed(&mut vitals, SelfVitals::default());
    }
}

/// Forgets the weather once there is no session to have had it.
///
/// The resource half of the same rule [`forget_vitals_without_a_session`] states: a sky a
/// session ended under is not this session's sky, and a reconnect that inherited it would
/// draw the last server's rain over the next server's desert until its first snapshot
/// landed.
fn forget_weather_without_a_session(session: Option<Res<Session>>, mut weather: ResMut<Weather>) {
    if session.is_none() {
        set_if_changed(&mut weather, Weather::default());
    }
}

/// Puts every entity the session can see where the interpolation says it is.
///
/// One entity per identity the server sent, spawned when it first appears and despawned
/// when it stops appearing. The **latest snapshot is the whole truth** about what this
/// session can see: an entity the server has stopped mentioning has left the view distance,
/// and keeping its body would leave a ghost standing where it was last seen.
fn apply_snapshots(
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    party: Res<Party>,
    mut dressing: Dressing<'_>,
    mut appearances: ResMut<Appearances>,
    mut existing: Query<(Entity, &Body, &mut Transform, &mut WalkPose)>,
    mut commands: Commands,
) {
    // Both exist from the first frame after startup. A frame without them is a frame before
    // there is a session, and there is nothing to place a body relative to.
    let (Some(session), Some(mut wardrobe)) = (session, dressing.wardrobe()) else {
        return;
    };

    let now = Instant::now();
    let interval = tick_interval(session.0.tick_rate);
    let mut drawn = buffer.sample(now, interval);

    // **Residents are drawn here, on the rig, and appended to the same list.** They ride in
    // the snapshot's `MobState` vector — `MobKind::Villager`, which is why
    // `mobs::MobVisuals::of` answers `None` for it — but a resident is a person, and every
    // rule below is already the rule a person needs: the newest snapshot is the existence
    // set, the description may arrive on either side of the body, and the cache is the size
    // of a view. A second loop would be a second copy of all three.
    //
    // **Nothing here can put a resident in a fall pose.** `player_is_dead` reads
    // `dead_players`, which the server fills with players; a resident's id is derived with
    // bit 62 set rather than minted from a counter, so it can never appear there — the
    // disjointness `server/internal/game/resident.go` argues, not a condition on this line.
    drawn.extend(buffer.sample_residents(now, interval));

    // The world is the authority on which bodies exist, rather than a map kept beside it.
    // A map would be a second copy of the same fact and could drift from it — a despawned
    // entity still recorded, or a recorded entity that was never spawned. Scanning a handful
    // of bodies per frame is cheaper than that class of bug.
    let mut placed = HashSet::with_capacity(drawn.len());
    for (entity, body, mut transform, mut walk) in &mut existing {
        match drawn.iter().find(|(entity_id, _)| *entity_id == body.0) {
            Some((_, state)) => {
                *transform = placement(state);
                let next = WalkPose::from(state);
                if *walk != next {
                    *walk = next;
                }
                placed.insert(body.0);
            }
            None => commands.entity(entity).despawn(),
        }
    }

    for (entity_id, state) in &drawn {
        if placed.contains(entity_id) {
            continue;
        }
        // **The placeholder is a rendering answer and never a decoding one.** An entity
        // can legitimately be visible before the message describing it lands, because the
        // two streams are not ordered against each other — so it is drawn in the neutral
        // grey `schemas/player.fbs` documents and `dress_bodies` replaces that in place
        // the moment the appearance arrives. Nothing pops out and respawns.
        let described = appearances.0.get_mut(entity_id);
        let description = match described {
            Some(described) => {
                described.drawn = true;
                Some(&*described)
            }
            None => None,
        };

        spawn_body(
            &mut commands,
            &mut wardrobe,
            *entity_id,
            session.0.entity_id,
            description,
            state,
            buffer.player_is_dead(*entity_id),
        );
    }

    // **What keeps two caches the size of a view rather than of a session.** An entity in
    // the newest snapshot keeps its entry; one that has left loses it, and the server
    // describes it again if it comes back — `Player.described` on the far side drops its
    // own entry for the same reason, so the two agree without either being told. An entry
    // that has never had a body is the one case a snapshot cannot answer, and it is held
    // for [`APPEARANCE_GRACE`] and no longer.
    appearances.0.retain(|entity_id, described| {
        drawn.iter().any(|(visible, _)| visible == entity_id)
            || party
                .members
                .iter()
                .any(|member| member.entity_id == *entity_id)
            || (!described.drawn && now.duration_since(described.at) < APPEARANCE_GRACE)
    });

    // **The palette is swept against what it could possibly need, not against whether the
    // cache changed size.** Every cached appearance can justify one colour per part, plus
    // the placeholder's; more than that and the map is certainly holding something nothing
    // wears. A body that changes its clothes without leaving is the case a size comparison
    // misses entirely — the cache is the same length afterwards, and the colour it stopped
    // wearing stays for the rest of the session.
    //
    // One integer comparison per frame and a scan only when it fails, so the common frame
    // costs nothing and the map is allowed a little slack before it is cleaned. What it is
    // not allowed is to grow: the ceiling moves with the cache, and the cache is the size
    // of a view.
    let per_description = BodyPart::IN_DRAWING_ORDER.len() + ArmourPiece::ALL.len();
    let justified = (appearances.0.len() + 1) * per_description;
    if wardrobe.palette.0.len() > justified {
        let live: HashSet<BodyMaterialKey> = appearances
            .0
            .values()
            .flat_map(|described| {
                BodyPart::IN_DRAWING_ORDER
                    .map(|part| BodyMaterialKey::appearance(part.colour(described.appearance)))
                    .into_iter()
                    .chain(Worn::described(described).material_keys())
            })
            .chain(
                BodyPart::IN_DRAWING_ORDER
                    .map(|part| BodyMaterialKey::appearance(part.colour(PLACEHOLDER_APPEARANCE))),
            )
            .collect();
        wardrobe.palette.0.retain(|key, _| live.contains(key));
    }
}

/// Puts every description the net thread decoded into the cache, newest last.
///
/// Runs **before** [`apply_snapshots`], so a body spawned on the frame its description
/// arrives is dressed on that frame rather than showing a placeholder for one of them.
/// Whether an entity has been drawn survives an update, because it is a fact about this
/// client and not about the message.
///
/// **Both queues, one system.** A resident is described by its own message with its own
/// fields, and every rule about *when* a description may be written, replaced or forgotten
/// is identical — so the two differ only in how a [`Described`] is built.
fn ingest_appearances(
    mut inbox: ResMut<AppearanceInbox>,
    mut residents: ResMut<ResidentInbox>,
    mut appearances: ResMut<Appearances>,
) {
    let players = inbox.take();
    let arrived_residents = residents.take();
    if players.is_empty() && arrived_residents.is_empty() {
        return;
    }

    let now = Instant::now();
    for message in players {
        remember(
            &mut appearances,
            now,
            message.entity_id,
            Described {
                appearance: message.appearance,
                name: message.name,
                label: PlateLabel::Level(message.level),
                worn_head: message.worn_head,
                worn_chest: message.worn_chest,
                worn_legs: message.worn_legs,
                worn_offhand: message.worn_offhand,
                at: now,
                drawn: false,
            },
        );
    }
    for message in arrived_residents {
        remember(
            &mut appearances,
            now,
            message.entity_id,
            Described {
                appearance: message.appearance,
                name: message.name,
                label: PlateLabel::Role(message.role),
                // **Zero on all four, a fact rather than a default.** V25's
                // `ResidentAppearance` carries no equipment, so there is nothing to read
                // and nothing to invent: no armour overlay is ever spawned over one.
                worn_head: 0,
                worn_chest: 0,
                worn_legs: 0,
                worn_offhand: 0,
                at: now,
                drawn: false,
            },
        );
    }
}

/// Writes one description, keeping what belongs to this client rather than to the message.
///
/// **The newest description wins and the clock does not restart.** A server correcting
/// itself is ordinary, so everything it said is replaced; `at` is not, because it is when
/// this entity was *first* described with nothing to draw it on and that is what
/// [`APPEARANCE_GRACE`] is a grace on. Refreshing it would hand the sender the bound: an
/// entity that never appears in a snapshot, named again inside every window, would live as
/// long as the connection did. `drawn` is kept for the same reason — a body exists or it
/// does not, and a second message about it does not change which.
fn remember(appearances: &mut Appearances, now: Instant, entity_id: u64, next: Described) {
    match appearances.0.get_mut(&entity_id) {
        Some(described) => {
            let (at, drawn) = (described.at, described.drawn);
            *described = Described { at, drawn, ..next };
        }
        None => {
            appearances
                .0
                .insert(entity_id, Described { at: now, ..next });
        }
    }
}

/// Dresses every body whose appearance has changed since it was drawn.
///
/// **In place, and that is the acceptance criterion**: an entity whose appearance arrives
/// after it does keeps its identity, its transform and its interpolation, and swaps the
/// existing piece handles. Despawning and respawning it would restart both and blink the
/// body.
///
/// It is also what makes a *changed* appearance free: the comparison against [`Worn`] is
/// one equality per body per frame, and the loop below runs only for the bodies where it
/// failed.
fn dress_bodies(
    appearances: Res<Appearances>,
    mut dressing: Dressing<'_>,
    mut bodies: Query<(Entity, &Body, &mut Worn, &Children)>,
    mut parts: Query<
        (
            &BodyVisual,
            &mut Mesh3d,
            &mut MeshMaterial3d<StandardMaterial>,
        ),
        Without<ArmourVisual>,
    >,
    mut overlays: Query<
        (
            Entity,
            &ArmourVisual,
            &mut Mesh3d,
            &mut MeshMaterial3d<StandardMaterial>,
        ),
        Without<BodyVisual>,
    >,
    shields: Query<Entity, With<ShieldVisual>>,
    mut commands: Commands,
) {
    let Some(mut wardrobe) = dressing.wardrobe() else {
        return;
    };

    for (owner, body, mut worn, children) in &mut bodies {
        let Some(described) = appearances.0.get(&body.0) else {
            continue;
        };
        let next = Worn::described(described);
        if next == *worn {
            continue;
        }
        let appearance_changed = next.appearance != worn.appearance;
        *worn = next;

        let outfit = wardrobe.outfit(next.appearance);
        let armour = wardrobe.armour(next);
        let shield = wardrobe.shield(next);
        let mut present = HashSet::with_capacity(armour.len());
        let current_shield = children.iter().find(|child| shields.contains(*child));
        match (current_shield, shield) {
            (Some(entity), None) => commands.entity(entity).despawn(),
            (None, Some((mesh, material))) => {
                commands.entity(owner).with_children(|parent| {
                    parent.spawn((
                        ShieldVisual,
                        Mesh3d(mesh),
                        MeshMaterial3d(material),
                        shield_pose(false),
                    ));
                });
            }
            _ => {}
        }

        for child in children {
            if let Ok((visual, mut mesh, mut material)) = parts.get_mut(*child) {
                if !appearance_changed {
                    continue;
                }
                let Some((_, shape, colour)) =
                    outfit.iter().find(|(piece, _, _)| *piece == visual.0)
                else {
                    continue;
                };
                if mesh.0 != *shape {
                    mesh.0 = shape.clone();
                }
                if material.0 != *colour {
                    material.0 = colour.clone();
                }
                continue;
            }

            let Ok((entity, visual, mut mesh, mut material)) = overlays.get_mut(*child) else {
                continue;
            };
            let Some((_, shape, colour)) =
                armour.iter().find(|(segment, _, _)| *segment == visual.0)
            else {
                commands.entity(entity).despawn();
                continue;
            };
            present.insert(visual.0);
            if mesh.0 != *shape {
                mesh.0 = shape.clone();
            }
            if material.0 != *colour {
                material.0 = colour.clone();
            }
        }

        commands.entity(owner).with_children(|parent| {
            for (segment, mesh, material) in armour {
                if present.contains(&segment) {
                    continue;
                }
                parent.spawn((
                    ArmourVisual(segment),
                    Mesh3d(mesh),
                    MeshMaterial3d(material),
                    resting_piece_transform(segment.body_piece()),
                ));
            }
        });
    }
}

/// Drops both body caches when there is no session.
///
/// The mirror of [`forget_vitals_without_a_session`], and the same argument: what a player
/// in a session that has ended looked like is not a fact about the next session, and
/// leaving it behind would have that one inherit it. A reconnect refills both, because the
/// server describes every entity to every session that has not been told yet.
///
/// Guarded rather than unconditional, because this runs on every frame for the rest of the
/// app's life and clearing an empty map still marks the resource changed.
fn forget_bodies_without_a_session(
    session: Option<Res<Session>>,
    mut appearances: ResMut<Appearances>,
    mut palette: ResMut<BodyMaterials>,
) {
    if session.is_some() || (appearances.0.is_empty() && palette.0.is_empty()) {
        return;
    }
    appearances.0.clear();
    palette.0.clear();
}

/// The transform one interpolated state becomes.
///
/// The translation is the **feet** position the snapshot carries — every part of the rig
/// is authored from the ground up, so nothing here has to offset anything, and the camera
/// can add an eye height to the same number.
fn placement(state: &interpolate::Interpolated) -> Transform {
    Transform {
        translation: state.pos,
        rotation: Quat::from_rotation_y(state.yaw),
        ..default()
    }
}

/// Spawns the entity that draws one of the server's entities.
///
/// Everybody gets one child per independently moving piece under the one body transform.
/// Static pieces keep the feet as their origin; limb meshes are authored relative to the
/// shoulder or hip carried by their child transform, so rotating one cannot orbit a leg
/// around its centre.
///
/// **This session's own player is drawn the same way and simply hidden in first person.**
/// It used to get no mesh, no children and no [`Worn`] at all — the camera sits at its
/// eyes, so a body there fills the screen with the inside of its own head. #172 gave that
/// camera somewhere else to be, and a body that exists but is invisible is a much smaller
/// thing than a second spawn path: `dress_bodies` needs the `Worn` it was denied,
/// `show_the_local_body` toggles a `Visibility` the renderer already honours, and toggling
/// the view therefore cannot respawn anything.
fn spawn_body(
    commands: &mut Commands,
    wardrobe: &mut Wardrobe<'_>,
    entity_id: u64,
    local_entity_id: u64,
    description: Option<&Described>,
    state: &interpolate::Interpolated,
    dead: bool,
) {
    let local = entity_id == local_entity_id;
    let (worn, name_plate) = match description {
        Some(description) => (
            Worn::described(description),
            Some((description.name.as_str(), description.label)),
        ),
        None => (Worn::bare(PLACEHOLDER_APPEARANCE), None),
    };
    let parts = wardrobe.outfit(worn.appearance);
    let armour = wardrobe.armour(worn);
    let shield = wardrobe.shield(worn);
    let placed = placement(state);
    let walk = WalkPose::from(state);
    let owner = commands
        .spawn((
            Body(entity_id),
            worn,
            walk,
            camera::DeathFall::newly_seen(dead),
            placed,
            // Every rendered child carries `InheritedVisibility`, whose hierarchy
            // propagation requires the parent to carry it too. Remote players and
            // residents keep this inherited value; the local body overrides it below.
            Visibility::Inherited,
        ))
        .id();
    if local {
        // Hidden until `show_the_local_body` says otherwise, which is the honest starting
        // value: the client starts in first person.
        commands
            .entity(owner)
            .insert((LocalPlayer, Visibility::Hidden));
    } else if let Some((name, label)) = name_plate {
        spawn_name_plate(commands, entity_id, name, label);
    }
    commands.entity(owner).with_children(|parent| {
        for (piece, mesh, material) in parts {
            parent.spawn((
                BodyVisual(piece),
                Mesh3d(mesh),
                MeshMaterial3d(material),
                resting_piece_transform(piece),
            ));
        }
        for (segment, mesh, material) in armour {
            parent.spawn((
                ArmourVisual(segment),
                Mesh3d(mesh),
                MeshMaterial3d(material),
                resting_piece_transform(segment.body_piece()),
            ));
        }
        if let Some((mesh, material)) = shield {
            parent.spawn((
                ShieldVisual,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                shield_pose(false),
            ));
        }
    });
}

fn shield_pose(raised: bool) -> Transform {
    let anchor = BodyPiece::LeftFist.pivot() + Vec3::new(-0.08, 0.02, -0.10);
    let rotation = if raised {
        Quat::from_rotation_x(-1.02) * Quat::from_rotation_z(0.18)
    } else {
        Quat::from_rotation_x(-0.25) * Quat::from_rotation_z(-0.28)
    };
    Transform::from_translation(anchor).with_rotation(rotation)
}

fn pose_body_shields(
    buffer: Res<SnapshotBuffer>,
    bodies: Query<(&Body, &Children)>,
    mut shields: Query<&mut Transform, With<ShieldVisual>>,
) {
    for (body, children) in &bodies {
        let next = shield_pose(buffer.player_is_blocking(body.0));
        for child in children {
            if let Ok(mut transform) = shields.get_mut(*child) {
                *transform = next;
            }
        }
    }
}

/// Bounds hostile display text before it reaches Bevy's layout engine.
///
/// The value remains display-only: it is never matched, parsed or used as identity.
/// Controls become the replacement glyph because even `no_wrap` honours hard newlines;
/// leaving them intact would let one name create an unbounded stack of lines. Truncation
/// walks Unicode scalars, so it can never split UTF-8. A combining sequence may end at the
/// boundary and remains valid text — no grapheme dependency is introduced for cosmetics.
/// The separator between the level and the name.
///
/// A vertical bar rather than `·` (U+00B7), for the same reason the mark below is three
/// full stops: Bevy's `default_font` is a 95-glyph ASCII subset of FiraMono, and a middle
/// dot is not one of the 95 - it laid out with zero advance, so the plate read
/// `Lv 7  Eivor` and the two fields ran together with nothing between them.
const NAME_PLATE_SEPARATOR: &str = " | ";

/// The mark a shortened name ends with, spelled in ASCII and paid for out of the bound.
///
/// `…` is absent from the same font, so a name that had been shortened read as a name that
/// simply ended there. The three characters come out of [`NAME_PLATE_CHARACTERS`] rather
/// than being added to it, so the plate is no wider than it was — and if the level prefix
/// ever left fewer than three characters for the name, the mark is what gives way, so the
/// bound holds for every prefix rather than only for the ones a `u16` level can produce.
const NAME_PLATE_TRUNCATION_MARK: &str = "...";

/// What a control character in a hostile name is shown as.
///
/// U+FFFD is the conventional answer and is missing from this font too, so it replaced a
/// character the layout engine must not see with one nothing draws. A question mark takes
/// up its column.
const NAME_PLATE_CONTROL_MARK: char = '?';

/// The two lines a plate can read: `Lv 7 | Eivor` for a player, `Bjorn | Smith` for a
/// resident.
///
/// Both are bounded by [`NAME_PLATE_CHARACTERS`] the same way, with the fixed part paid for
/// out of the bound rather than added to it: a plate is a fixed-width box and the
/// truncation mark is what gives way. **The name is the only untrusted half of either**,
/// which is why it is the only half truncated or stripped of control characters — a role is
/// one of six words this build wrote.
fn name_plate_text(label: PlateLabel, name: &str) -> String {
    let (prefix, suffix) = match label {
        PlateLabel::Level(level) => (format!("Lv {level}{NAME_PLATE_SEPARATOR}"), String::new()),
        PlateLabel::Role(role) => (
            String::new(),
            format!("{NAME_PLATE_SEPARATOR}{}", role_label(role)),
        ),
    };
    let name_characters = NAME_PLATE_CHARACTERS
        .checked_sub(prefix.chars().count() + suffix.chars().count())
        .expect("a u16 level prefix or a role label fits inside the name-plate bound");
    let mut shown = String::with_capacity(NAME_PLATE_CHARACTERS * 4);
    shown.push_str(&prefix);
    // One character past the bound is what makes this a truncation rather than a fit: a
    // name of exactly `name_characters` characters is shown whole.
    let head: Vec<char> = name
        .chars()
        .take(name_characters.saturating_add(1))
        .collect();
    let displayable = |character: char| {
        if char::is_control(character) {
            NAME_PLATE_CONTROL_MARK
        } else {
            character
        }
    };
    if head.len() <= name_characters {
        shown.extend(head.into_iter().map(displayable));
        shown.push_str(&suffix);
        return shown;
    }
    let kept = name_characters.saturating_sub(NAME_PLATE_TRUNCATION_MARK.chars().count());
    shown.extend(head.into_iter().take(kept).map(displayable));
    shown.extend(NAME_PLATE_TRUNCATION_MARK.chars().take(name_characters));
    shown.push_str(&suffix);
    shown
}

fn name_plate_name(name: &str) -> String {
    name_plate_text(PlateLabel::Level(0), name)
        .strip_prefix(&format!("Lv 0{NAME_PLATE_SEPARATOR}"))
        .unwrap_or(name)
        .to_owned()
}

fn spawn_name_plate(commands: &mut Commands, entity_id: u64, name: &str, label: PlateLabel) {
    commands.spawn((
        NamePlate(entity_id),
        PlateSight::default(),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Px(NAME_PLATE_WIDTH),
            height: Val::Px(NAME_PLATE_HEIGHT),
            padding: UiRect::horizontal(Val::Px(6.0)),
            overflow: Overflow::clip(),
            border_radius: BorderRadius::all(Val::Px(5.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.72)),
        Text::new(name_plate_text(label, name)),
        TextFont {
            font_size: FontSize::Px(NAME_PLATE_FONT_SIZE),
            ..default()
        },
        TextColor(DEFAULT_PLATE_COLOUR),
        TextLayout::no_wrap().with_justify(Justify::Center),
        TextShadow::default(),
        FocusPolicy::Pass,
        GlobalZIndex(8),
        // A plate is shown only after a camera successfully projects its head anchor.
        Visibility::Hidden,
    ));
}

/// Reconciles the UI labels with the bodies and the server descriptions already cached.
///
/// A body can precede its `PlayerAppearance`, so spawning only in [`spawn_body`] would
/// permanently miss that ordinary ordering. Conversely, a plate is a UI root rather than
/// a body child and must be removed explicitly when the complete snapshot drops its body.
/// The local entity is deliberately omitted in both views: first person has no visible
/// head to label, and third person does not need to tell the player their own name.
fn sync_name_plates(
    session: Option<Res<Session>>,
    appearances: Res<Appearances>,
    party: Res<Party>,
    bodies: Query<&Body>,
    mut plates: Query<(Entity, &NamePlate, &mut Text, &mut TextColor)>,
    mut commands: Commands,
) {
    let Some(local_entity_id) = session.map(|session| session.0.entity_id) else {
        for (entity, _, _, _) in &mut plates {
            commands.entity(entity).despawn();
        }
        return;
    };

    let body_ids: HashSet<u64> = bodies.iter().map(|body| body.0).collect();
    let mut existing = HashSet::with_capacity(body_ids.len());
    for (entity, plate, mut text, mut colour) in &mut plates {
        let described = appearances.0.get(&plate.0);
        if plate.0 == local_entity_id || !body_ids.contains(&plate.0) || described.is_none() {
            commands.entity(entity).despawn();
            continue;
        }
        existing.insert(plate.0);
        let described = described.expect("checked above");
        let next = name_plate_text(described.label, &described.name);
        if text.0 != next {
            text.0 = next;
        }
        let next_colour = if party
            .members
            .iter()
            .any(|member| member.entity_id == plate.0)
        {
            PARTY_PLATE_COLOUR
        } else {
            DEFAULT_PLATE_COLOUR
        };
        if colour.0 != next_colour {
            colour.0 = next_colour;
        }
    }

    for entity_id in body_ids {
        if entity_id == local_entity_id || existing.contains(&entity_id) {
            continue;
        }
        if let Some(described) = appearances.0.get(&entity_id) {
            spawn_name_plate(&mut commands, entity_id, &described.name, described.label);
        }
    }
}

/// The feet-relative point whose projection the plate sits above.
fn name_plate_anchor(body: &Transform) -> Vec3 {
    let envelope = body_envelope();
    let top = envelope.centre.y + envelope.size.y / 2.0 + NAME_PLATE_GAP;
    body.transform_point(Vec3::Y * top)
}

/// The limit this plate is currently judged against, in blocks.
///
/// The band that makes the distance rule stable: a plate the distance rule currently admits
/// keeps its full [`NAME_PLATE_DISTANCE`], one it currently rejects has to come
/// [`NAME_PLATE_DISTANCE_MARGIN`] further in before it is admitted again. Reading the
/// current answer to pick the threshold is the whole of the hysteresis; there is no state
/// here beyond the `near` flag the caller already holds.
///
/// **`near`, emphatically not `shown`.** The argument is the distance rule's own previous
/// answer — see [`PlateSight`] for what choosing the drawn state instead would cost.
fn name_plate_reach(near: bool) -> f32 {
    if near {
        NAME_PLATE_DISTANCE
    } else {
        NAME_PLATE_DISTANCE - NAME_PLATE_DISTANCE_MARGIN
    }
}

/// Whether nothing solid stands between the camera and the point a plate is drawn above.
///
/// **A drawing decision, not a gameplay one.** The server decided long before this what
/// this session may know: it streams chunks and entities by its own visibility rule, and
/// the names are legitimately in the snapshot. All that is chosen here is whether to paint
/// a label over the player's own view, exactly as the failed projection below already
/// chooses. Nothing is sent, and no outcome depends on the answer.
///
/// It reuses [`target::raycast`] rather than restating a traversal. That is the point: the
/// server's `clearLineOfSight` is authoritative for gameplay and this is not, so the client
/// half must not become a second hand-written walk that can disagree with the one already
/// driving the aiming outline and the camera boom. `solid` is a predicate for the same
/// reason `camera::boom_length` takes one — what this needs to know is whether a voxel
/// stops light, and a test that has to assemble a chunk store to put a wall somewhere is a
/// test about chunk stores.
///
/// The anchor's own voxel is excluded, and that is a correctness fix rather than a
/// tolerance. The anchor sits a hand's width above the head, so for anybody standing under
/// a ceiling it is often *inside* the ceiling block; a ray that counted the voxel it
/// terminates in would hide the plate of somebody standing plainly in front of you indoors.
/// Anything genuinely between the two endpoints is hit first and still wins.
fn name_plate_line_is_clear(eye: Vec3, anchor: Vec3, mut solid: impl FnMut(IVec3) -> bool) -> bool {
    let to_anchor = anchor - eye;
    let anchor_voxel = anchor.floor().as_ivec3();
    target::raycast(eye, to_anchor, to_anchor.length(), |voxel| {
        voxel != anchor_voxel && solid(voxel)
    })
    .is_none()
}

/// Whether the player could see the owner of a plate whose distance rule last answered
/// `near`, and what the distance rule answers this frame.
///
/// The two rules, in the order they have to run and nowhere else in the file. They are
/// independent: a plate near enough but behind rock fails on the second, a plate on a
/// perfectly clear line but too far away fails on the first, and neither can mask the other
/// because `&&` is the only thing joining them.
///
/// **The returned `near` is what makes that independence hold over time rather than only
/// within a frame.** The `&&` keeps either rule from masking the other's *hiding*; carrying
/// the distance answer back out — instead of letting the caller re-derive it from the drawn
/// state — is what keeps either from masking the other's *showing*. [`PlateSight`] records
/// what the coupled version cost.
///
/// **Distance first, and the ordering is load-bearing rather than tidy.** This runs per
/// plate per frame, the traversal is the expensive half, and the rejection is a subtraction
/// and a comparison. So the ray only ever walks for the handful of plates that are near
/// enough to be drawn at all.
fn name_plate_is_in_sight(
    eye: Vec3,
    anchor: Vec3,
    near: bool,
    solid: impl FnMut(IVec3) -> bool,
) -> (bool, bool) {
    let near = eye.distance(anchor) <= name_plate_reach(near);
    (near, near && name_plate_line_is_clear(eye, anchor, solid))
}

/// Advances one plate's hysteresis by a frame and answers what it should now do.
///
/// Pure, and takes the state by value, so the caller can write it back only when it moved
/// and the boundary behaviour can be tested a frame at a time without an app.
fn settle_plate_sight(sight: PlateSight, wanted: bool) -> PlateSight {
    if wanted == sight.shown {
        return PlateSight { dwell: 0, ..sight };
    }
    let dwell = sight.dwell.saturating_add(1);
    if dwell >= NAME_PLATE_SIGHT_DWELL {
        PlateSight {
            shown: wanted,
            dwell: 0,
            ..sight
        }
    } else {
        PlateSight { dwell, ..sight }
    }
}

/// Projects each head anchor into the UI viewport, for the plates the player could see.
///
/// Screen-space text is the deliberate choice over world text: its pixel size remains
/// legible everywhere a plate is drawn and the existing Bevy feature set already owns the
/// font and UI renderer. A failed projection means behind the camera, off-screen, or not
/// ready yet, and hides rather than clamps the plate to an unrelated screen edge.
///
/// Three rules hide a plate and they are independent. Beyond [`NAME_PLATE_DISTANCE`] it is
/// hidden however clear the line; with solid terrain between the camera and the anchor it
/// is hidden however close; and a projection that fails hides it as it always has. The first
/// two are steadied by a mechanism each — the distance rule by its own band, carried in
/// `PlateSight::near`, and the occlusion rule by the dwell in [`settle_plate_sight`] — so
/// neither boundary can strobe and neither steadies or holds back the other. The third stays
/// immediate, because it is not a boundary a player stands on — it is the anchor leaving the
/// frame.
fn position_name_plates(
    session: Option<Res<Session>>,
    store: Option<Res<ChunkStore>>,
    cameras: Query<(&Camera, &Transform), With<WorldCamera>>,
    bodies: Query<(&Body, &Transform)>,
    mut plates: Query<(&NamePlate, &mut PlateSight, &mut Node, &mut Visibility)>,
) {
    let Ok((camera, camera_transform)) = cameras.single() else {
        for (_, _, _, mut visibility) in &mut plates {
            *visibility = Visibility::Hidden;
        }
        return;
    };
    let eye = camera_transform.translation;
    let camera_transform = GlobalTransform::from(*camera_transform);

    // Both or neither, and an absent pair stops no light: the chunk size is what turns a
    // voxel coordinate into a chunk, so a store with no session to size it cannot be looked
    // up in at all. A chunk that has not arrived is not solid — the same answer
    // `ChunkStore::solid_at` gives the aiming ray and the camera boom, and honest for the
    // same reason: this client knows nothing about it.
    let voxels = session
        .as_deref()
        .zip(store.as_deref())
        .map(|(session, store)| (store, usize::from(session.0.chunk_size)));

    for (plate, mut sight, mut node, mut visibility) in &mut plates {
        let Some((_, body)) = bodies.iter().find(|(body, _)| body.0 == plate.0) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let anchor = name_plate_anchor(body);
        let (near, wanted) = name_plate_is_in_sight(eye, anchor, sight.near, |voxel| {
            voxels.is_some_and(|(store, size)| {
                store.solid_at(
                    BlockCoord {
                        x: voxel.x,
                        y: voxel.y,
                        z: voxel.z,
                    },
                    size,
                )
            })
        });

        // The distance answer is written back before the dwell runs, and it is not filtered
        // by it. Its own band is what steadies it, so passing it through the occlusion
        // filter as well would only make the two rules share a mechanism again.
        let next = settle_plate_sight(PlateSight { near, ..*sight }, wanted);
        if *sight != next {
            *sight = next;
        }
        if !next.shown {
            *visibility = Visibility::Hidden;
            continue;
        }

        match camera.world_to_viewport(&camera_transform, anchor) {
            Ok(screen) => {
                node.left = Val::Px(screen.x - NAME_PLATE_WIDTH / 2.0);
                node.top = Val::Px(screen.y - NAME_PLATE_HEIGHT);
                *visibility = Visibility::Inherited;
            }
            Err(_) => *visibility = Visibility::Hidden,
        }
    }
}

pub(crate) fn resting_piece_transform(piece: BodyPiece) -> Transform {
    Transform::from_translation(piece.pivot())
}

/// Swings every body's limbs from the distance its interpolated snapshots covered.
///
/// Local and remote bodies share this system and no input resource appears in its
/// signature. Arms counter-swing the opposite legs. A dead body is put in its neutral
/// child pose before [`collapse_bodies`] tips the root, so the two animations never
/// compose competing limb rotations.
fn animate_walking_bodies(
    buffer: Res<SnapshotBuffer>,
    bodies: Query<(&Body, &WalkPose, &Children)>,
    mut parts: Query<(&BodyVisual, &mut Transform), Without<ArmourVisual>>,
    mut armour: Query<(&ArmourVisual, &mut Transform), Without<BodyVisual>>,
) {
    for (body, walk, children) in &bodies {
        let blocking = buffer.player_is_blocking(body.0);
        let stride = if walk.moving && !buffer.player_is_dead(body.0) {
            walk.phase.sin() * WALK_SWING
        } else {
            0.0
        };

        for child in children {
            if let Ok((visual, mut transform)) = parts.get_mut(*child) {
                apply_walk_transform(visual.0, stride, blocking, &mut transform);
            } else if let Ok((visual, mut transform)) = armour.get_mut(*child) {
                apply_walk_transform(visual.0.body_piece(), stride, blocking, &mut transform);
            };
        }
    }
}

fn apply_walk_transform(piece: BodyPiece, stride: f32, blocking: bool, transform: &mut Transform) {
    let angle = match piece.limb() {
        Some(Limb::LeftArm) if blocking => -1.05,
        Some(Limb::LeftLeg | Limb::RightArm) => stride,
        Some(Limb::RightLeg | Limb::LeftArm) => -stride,
        None => 0.0,
    };
    let next = Transform {
        translation: piece.pivot(),
        rotation: Quat::from_rotation_x(angle),
        ..default()
    };
    if *transform != next {
        *transform = next;
    }
}

/// Tips every body the newest snapshot says is dead.
///
/// `EntitySnapshot.dead_players` is the only input. Health, missing entities and local
/// vitals decide nothing here: the server names a body dead, this presents the existing
/// fall, and the same body stands upright on the first snapshot that stops naming it.
///
/// **One code path for the viewer and everybody beside them.** The local body carries the
/// same [`camera::DeathFall`] component every remote body does, and the first-person camera
/// reads that component after this set. Its eye and its third-person rig therefore share one
/// clock rather than two implementations of the same fall.
///
/// **Composed onto the snapshot's transform rather than stored**, which is what makes it
/// self-clearing: `apply_snapshots` rewrites the whole transform from the newest snapshot
/// every frame, so a respawn puts the body upright with nothing here having to animate it
/// back. A body first seen already dead starts at the end of the curve — the state is a
/// fact the viewer arrived after, not an event to replay from standing.
fn collapse_bodies(
    time: Res<Time>,
    buffer: Res<SnapshotBuffer>,
    mut bodies: Query<(&Body, &mut camera::DeathFall, &mut Transform)>,
) {
    for (body, mut fall, mut transform) in &mut bodies {
        let mut next = *fall;
        next.advance(buffer.player_is_dead(body.0), time.delta());
        if *fall != next {
            *fall = next;
        }
        transform.rotation *= Quat::from_rotation_x(DEATH_BODY_PITCH * next.fallen());
    }
}

/// Shows the local player's body exactly while the camera is not inside its head.
///
/// `Visibility` rather than spawning and despawning, so the body the player looks at in
/// third person is the same entity the snapshots have been driving all along — with the
/// appearance `dress_bodies` already put on it, and no frame of a bare figure while the
/// wardrobe catches up.
fn show_the_local_body(view: Res<ViewMode>, mut bodies: Query<&mut Visibility, With<LocalPlayer>>) {
    let next = if view.first_person() {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for mut visibility in &mut bodies {
        if *visibility != next {
            *visibility = next;
        }
    }
}

/// The authoritative selected stack and the local state that chooses its renderer.
///
/// Grouped because they are one subject: the item comes from the server-sent pack, while
/// the selected index and camera view decide only where that presentation appears.
#[derive(SystemParam)]
struct BodyHeldSubject<'w> {
    inventory: Res<'w, Inventory>,
    selected: Res<'w, SelectedSlot>,
    mode: Res<'w, InputMode>,
    view: Res<'w, ViewMode>,
    session: Option<Res<'w, Session>>,
}

impl BodyHeldSubject<'_> {
    fn item_id(&self) -> Option<u16> {
        stack_item_id(self.inventory.slot(self.selected.0))
    }

    fn visibility(&self) -> Visibility {
        if held_item_surface(*self.mode, *self.view, self.session.is_some())
            == HeldItemSurface::Body
        {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        }
    }
}

/// The shared world-space item assets as one borrow.
#[derive(SystemParam)]
struct BodyHeldAssets<'w> {
    visuals: Option<ResMut<'w, drops::DropVisuals>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
}

impl BodyHeldAssets<'_> {
    fn presentation(
        &mut self,
        item_id: u16,
    ) -> Option<(ItemShape, Handle<Mesh>, Handle<StandardMaterial>)> {
        let visuals = self.visuals.as_deref_mut()?;
        Some((
            item_shape(item_id),
            visuals.mesh_for(item_id),
            visuals.material_for(item_id, &mut self.materials),
        ))
    }

    /// The second piece a held item is drawn from, when it has one.
    ///
    /// **A sword's grip is wood and the blade's material is its steel**, so the world draws
    /// the two in separate materials — see `drops::sword_grip`. The body's fist takes the
    /// same world assets a drop does, so it takes both pieces or it holds a sword with a
    /// steel handle.
    fn second_piece(
        &mut self,
        shape: ItemShape,
    ) -> Option<(Handle<Mesh>, Handle<StandardMaterial>)> {
        let visuals = self.visuals.as_deref_mut()?;
        visuals.second_piece_for(shape, &mut self.materials)
    }
}

/// Mirrors the authoritative selected item into the local body's right hand.
///
/// The item is parented to the local body's right fist. Its mesh is the same world-scale
/// asset a drop of that shape uses, its colour comes through `player/items.rs`, and the
/// fist's shoulder pivot makes it inherit the distance-driven arm swing. Nothing here
/// states an item to the server or changes what it can do.
fn refresh_body_held_item(
    subject: BodyHeldSubject<'_>,
    mut assets: BodyHeldAssets<'_>,
    local_body: Query<&Children, With<LocalPlayer>>,
    body_parts: Query<&BodyVisual>,
    held: Query<(Entity, &BodyHeldItem, &ChildOf)>,
    held_visibility: Query<&Visibility, With<BodyHeldItem>>,
    mut commands: Commands,
) {
    let Some(children) = local_body.iter().next() else {
        return;
    };
    let Some(right_fist) = children.iter().find(|child| {
        body_parts
            .get(*child)
            .is_ok_and(|visual| visual.0 == BodyPiece::RightFist)
    }) else {
        return;
    };
    let selected_item = subject.item_id();
    let visibility = subject.visibility();

    let mut current = None;
    for (entity, item, parent) in &held {
        if parent.parent() == right_fist {
            current = Some((entity, *item));
            break;
        }
    }

    let Some(item_id) = selected_item else {
        if let Some((entity, _)) = current {
            commands.entity(entity).despawn();
        }
        return;
    };

    if let Some((entity, current_item)) = current
        && current_item.item_id == item_id
    {
        if held_visibility
            .get(entity)
            .is_ok_and(|current| *current != visibility)
        {
            commands.entity(entity).insert(visibility);
        }
        return;
    }

    let Some((shape, mesh, material)) = assets.presentation(item_id) else {
        return;
    };
    let second = assets.second_piece(shape);
    // **Rebuilt rather than added to.** A held item may gain or lose its second piece when
    // the selection changes — a sword has a wooden grip and a stone does not — so the
    // children are replaced wholesale, exactly as `ui::icon::redraw` replaces a cell's
    // rectangles. Leaving a stale grip under a block is the failure this shape removes.
    let dress = move |commands: &mut Commands<'_, '_>, entity: Entity| {
        commands.entity(entity).despawn_related::<Children>();
        if let Some((mesh, material)) = second {
            commands.entity(entity).with_child((
                BodyHeldPiece,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
            ));
        }
    };

    if let Some((entity, _)) = current {
        commands.entity(entity).insert((
            BodyHeldItem { item_id, shape },
            Mesh3d(mesh),
            MeshMaterial3d(material),
            body_held_item_transform(shape),
        ));
        dress(&mut commands, entity);
        if held_visibility
            .get(entity)
            .is_ok_and(|current| *current != visibility)
        {
            commands.entity(entity).insert(visibility);
        }
        return;
    }

    let held = commands
        .spawn((
            BodyHeldItem { item_id, shape },
            Mesh3d(mesh),
            MeshMaterial3d(material),
            body_held_item_transform(shape),
            visibility,
            ChildOf(right_fist),
        ))
        .id();
    dress(&mut commands, held);
}

/// Republishes what the overlay reports.
///
/// Writes only on a change, because `ResMut` marks the resource changed on every
/// `DerefMut` — and the status line uses change detection to avoid rebuilding its string
/// every frame.
fn refresh_player_stats(
    buffer: Res<SnapshotBuffer>,
    cadence: Res<InputCadence>,
    bodies: Query<&Body>,
    session: Option<Res<Session>>,
    player: Query<&Transform, With<LocalPlayer>>,
    mut stats: ResMut<PlayerStats>,
) {
    let next = PlayerStats {
        position: player.iter().next().map(|transform| transform.translation),
        // From the snapshot's velocity rather than differenced from two positions: the
        // server's own number is exact, and differencing an interpolated position would
        // read zero every time two frames landed inside one tick.
        speed: session.and_then(|session| {
            buffer
                .velocity_of(session.0.entity_id)
                .map(|vel| Vec3::from_array(vel).length())
        }),
        entities: bodies.iter().count(),
        server_tick: buffer.latest_tick(),
        inputs_sent: cadence.sent,
        inputs_dropped: cadence.dropped,
    };

    if *stats != next {
        *stats = next;
    }
}

/// Writes where the player has got to, each time they have covered some ground.
///
/// It exists because the debug overlay is on the screen, and a screen is exactly what CI, a
/// remote session and an automated end-to-end check do not have — the mirror of
/// `log_when_meshing_settles` in `world/render.rs`. At `debug` level, so it costs nothing
/// unless somebody asks for it, and keyed on distance so standing still is silent.
fn log_the_players_progress(stats: Res<PlayerStats>, mut reported: Local<Option<Vec3>>) {
    let Some(position) = stats.position else {
        return;
    };

    let moved = match *reported {
        Some(last) => last.distance(position) >= MOVEMENT_LOG_DISTANCE,
        None => true,
    };
    if !moved {
        return;
    }
    *reported = Some(position);

    debug!(
        "player at {:.1}, {:.1}, {:.1} | tick {:?} | {} entities in view | {} inputs sent, {} dropped",
        position.x,
        position.y,
        position.z,
        stats.server_tick,
        stats.entities,
        stats.inputs_sent,
        stats.inputs_dropped,
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One server tick.
///
/// `tick_rate >= 1` is a decoder invariant — `SessionParams` cannot be constructed with a
/// zero, see `net/codec.rs` — so there is no reachable state in which this divides by it.
fn tick_interval(tick_rate: u8) -> Duration {
    Duration::from_secs(1) / u32::from(tick_rate)
}

/// Brings a yaw into (-π, π], matching what the server does to the one it echoes.
fn wrap_look(look: LookState) -> LookState {
    LookState {
        yaw: wrap_angle(look.yaw),
        pitch: look.pitch,
    }
}

fn wrap_angle(radians: f32) -> f32 {
    use std::f32::consts::{PI, TAU};

    let wrapped = (radians + PI).rem_euclid(TAU);
    wrapped - PI
}

/// Assigns only when the value differs.
///
/// `ResMut` marks a resource changed on every `DerefMut`, whether or not the value moved,
/// so an unconditional write from a per-frame system makes change detection useless for
/// everything downstream of it.
fn set_if_changed<T>(resource: &mut ResMut<'_, T>, next: T)
where
    T: Resource<Mutability = bevy::ecs::component::Mutable> + PartialEq,
{
    if **resource != next {
        **resource = next;
    }
}

/// Merges primitive parts into one mesh, reporting rather than unwrapping.
///
/// Unreachable in practice: every part this module's callers pass is a Bevy primitive, so
/// they all carry the same attributes in the same layout, which is the only thing `merge`
/// refuses. Reported for the reason the target outline's merge is — a body or a building
/// missing a part is a cosmetic fault, and taking the window down over one would not be.
///
/// It lives here rather than in one of its two callers because both
/// [`structures::create_visuals`] and [`mobs::create_visuals`] build a body out of several
/// primitives, and a copy in each is a second place for the error handling to drift.
fn merge_all(into: &mut Mesh, parts: impl IntoIterator<Item = Mesh>, what: &'static str) {
    for part in parts {
        if let Err(err) = into.merge(&part) {
            error!("the {what} mesh is missing a part: {err}");
        }
    }
}

/// Builds the rolled load and its two straps inside an exact outer bound.
///
/// The first-person hand and world drop pass different scales through this one shape
/// recipe, so a tent cannot regress to a box in one surface while remaining a roll in the
/// other. The returned meshes stay separate long enough for the roll to take the item's
/// colour and the straps to take their shared brown.
pub(super) fn rolled_bundle_parts(bounds: Vec3) -> (Mesh, Mesh) {
    const ROLL_HEIGHT_RATIO: f32 = 0.52 / 0.62;
    const ROLL_DEPTH_RATIO: f32 = 0.62 / 0.72;
    const STRAP_WIDTH_RATIO: f32 = 0.12 / 1.15;
    const STRAP_OFFSET_RATIO: f32 = 0.31 / 1.15;

    let cylinder = |size: Vec3| {
        Mesh::from(Cylinder::new(0.5, 1.0))
            .rotated_by(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2))
            .scaled_by(size)
    };
    let roll = cylinder(Vec3::new(
        bounds.x,
        bounds.y * ROLL_HEIGHT_RATIO,
        bounds.z * ROLL_DEPTH_RATIO,
    ));
    let strap_size = Vec3::new(bounds.x * STRAP_WIDTH_RATIO, bounds.y, bounds.z);
    let strap_offset = bounds.x * STRAP_OFFSET_RATIO;
    let mut straps = cylinder(strap_size).translated_by(Vec3::X * -strap_offset);
    let other = cylinder(strap_size).translated_by(Vec3::X * strap_offset);
    merge_all(&mut straps, [other], "packed-gear straps");
    (roll, straps)
}

/// The leather cord shared by every packed structure, as linear vertex/material colour.
pub(super) fn bundle_strap_linear_rgba() -> [f32; 4] {
    let colour = Color::srgb_u8(106, 67, 35).to_linear();
    [colour.red, colour.green, colour.blue, colour.alpha]
}

#[cfg(test)]
mod tests;
