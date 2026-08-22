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
//! | `interpolate.rs` | the two-snapshot buffer and the interpolation — pure, no Bevy world |
//! | `drops.rs` | authoritative drop spawn/despawn and cosmetic cube motion |
//! | `hands.rs` | the camera-space held item and its cosmetic swing |
//! | `items.rs` | one row per item id: what it is called, its held shape, its colour |
//! | `inventory.rs` | the server-sent slots and the locally selected slot index |
//! | `crafting.rs` | the display-only recipe mirror and the craft intent it originates |
//! | `camera.rs` | the one camera, and what it follows |
//! | `sky.rs` | the sun, the sky colour, the ambient term and the fog, on the server's clock |
//! | `target.rs` | the voxel raycast, mining intent/progress, placement and outline |
//! | `structures.rs` | the tents and forges a snapshot names, and the two requests for one |
//! | `constants.rs` | the numbers, and which of them mirror the server |
//! | `appearance.rs` | the rig: which box each appearance colour covers, and where it sits |

mod appearance;
mod camera;
mod combat;
mod constants;
mod crafting;
mod drops;
mod hands;
mod interpolate;
mod inventory;
mod items;
mod mobs;
mod sky;
mod structures;
mod target;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;

pub(crate) use appearance::{
    BodyPart, PlacedBox, boxes as body_boxes, envelope as body_envelope, placed as placed_box,
    slots as body_slots,
};

pub use crafting::{CraftClick, Ingredient, RECIPES, Recipe};
pub use interpolate::SnapshotBuffer;
pub use inventory::{
    ApplyInventory, Inventory, InventoryClick, InventoryClickKind, PickedStack, SelectedSlot,
};
pub use items::item_label;
#[cfg(test)]
pub(crate) use items::known_item_ids;
pub(crate) use items::{ItemShape, item_palette_id, item_shape};
pub use target::{ApplyMiningFeedback, MiningFeedback};

use crate::net::{
    Appearance, AppearanceInbox, HairModel, LifeState, Outbound, PLACEHOLDER_APPEARANCE,
    PlayerInput, PlayerVitals, Sent, Session, SnapshotInbox, encode_player_input,
};
use constants::{LOOK_SENSITIVITY, MAX_PITCH};

/// How far the player has to move before the movement log says so again, in blocks.
///
/// The log line exists because the debug overlay is on a screen, and a screen is what CI,
/// a remote session and an automated end-to-end check do not have — the same reason
/// `world/render.rs` logs when meshing settles. Keyed on distance rather than on time so a
/// player standing still is silent.
const MOVEMENT_LOG_DISTANCE: f32 = 8.0;

/// The hair model handed to [`body_boxes`] for a part that is not the hair, where it is
/// ignored.
///
/// Named rather than spelled at the call site, so nobody reads it as a default haircut:
/// `body_boxes` is total over the part, and every part but [`BodyPart::Hair`] has one
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
    /// Pointer released and the authoritative inventory visible.
    Inventory,
    /// Pointer released and the pause menu visible.
    Menu,
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
        app.init_resource::<LookState>()
            .init_resource::<MoveIntent>()
            .init_resource::<InputMode>()
            .init_resource::<InputCadence>()
            .init_resource::<SnapshotBuffer>()
            .init_resource::<Appearances>()
            .init_resource::<BodyMaterials>()
            .init_resource::<SelfVitals>()
            .init_resource::<sky::SkyClock>()
            .init_resource::<PlayerStats>()
            // `init_resource` rather than `insert_resource`, and `NetPlugin` does the same:
            // whichever plugin is built first creates the inbox and the other finds it.
            .init_resource::<SnapshotInbox>()
            .init_resource::<AppearanceInbox>()
            .add_systems(
                Startup,
                (
                    create_player_visuals,
                    drops::create_visuals,
                    mobs::create_visuals,
                    structures::create_visuals,
                    sky::spawn_sun,
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
                        ingest_snapshots,
                        // Before the bodies, so an entity whose appearance and first
                        // snapshot share a frame is dressed on that frame.
                        ingest_appearances,
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
                        drops::animate,
                        mobs::animate,
                        structures::animate,
                        refresh_player_stats,
                    )
                        .chain()
                        .in_set(ApplySnapshots),
                    log_the_players_progress.after(ApplySnapshots),
                    forget_vitals_without_a_session.after(ApplySnapshots),
                    forget_bodies_without_a_session.after(ApplySnapshots),
                )
                    .after(crate::net::DrainNetwork),
            )
            .add_plugins(camera::PlayerCameraPlugin)
            .add_plugins(inventory::InventoryPlugin)
            // After the inventory plugin, because the craft gate is read against the
            // newest complete state and the ordering inside `CraftingPlugin` is written
            // against its system set.
            .add_plugins(crafting::CraftingPlugin)
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
/// **Ten meshes for a whole settlement.** Every player is the same geometry and only the
/// colours differ, so a part is merged into one mesh the way a mob's body and a
/// structure's are — five for the parts whose shape is fixed, and one per hair model,
/// which is the one part whose shape a player chooses. Nothing here is ever rebuilt: a
/// body that changes its hair swaps a handle.
#[derive(Resource, Debug)]
struct PlayerVisuals {
    shoes: Handle<Mesh>,
    trousers: Handle<Mesh>,
    shirt: Handle<Mesh>,
    skin: Handle<Mesh>,
    eyes: Handle<Mesh>,
    /// One per model, paired with the model it draws. An array rather than a map because
    /// there are five of them and `HairModel` is deliberately not a number.
    hair: [(HairModel, Handle<Mesh>); HairModel::ALL.len()],
}

impl PlayerVisuals {
    /// The mesh one part of a body wearing one hair model is drawn from.
    ///
    /// Total over [`BodyPart`] with no wildcard arm, so a sixth part does not compile
    /// until it has been given geometry.
    fn mesh(&self, part: BodyPart, model: HairModel) -> Handle<Mesh> {
        match part {
            BodyPart::Shoes => self.shoes.clone(),
            BodyPart::Trousers => self.trousers.clone(),
            BodyPart::Shirt => self.shirt.clone(),
            BodyPart::Skin => self.skin.clone(),
            BodyPart::Eyes => self.eyes.clone(),
            BodyPart::Hair => self
                .hair
                .iter()
                .find(|(drawn, _)| *drawn == model)
                .map_or_else(|| self.skin.clone(), |(_, mesh)| mesh.clone()),
        }
    }
}

/// One material per colour, rather than one per player.
///
/// **Keyed on the colour itself, which is what makes twenty players in view cost twenty
/// bodies and not a hundred materials**: two people in the same walnut tunic share one
/// `StandardMaterial`, and the palettes the character screen offers bound how many there
/// can be. The key is the wire's `0x00RRGGBB`, so the value a server sent is the value
/// this map is asked about — no rounding, and nothing to disagree about.
///
/// Swept by [`apply_snapshots`] rather than grown for ever: a server is free to describe
/// a colour nobody can choose, and sixteen million of them is a map. An entry dropped
/// while a body still wears it costs nothing, because the body holds a strong handle to
/// the material and the next one asking for that colour simply makes it again.
#[derive(Resource, Debug, Default)]
struct BodyMaterials(HashMap<u32, Handle<StandardMaterial>>);

impl BodyMaterials {
    /// The material for one colour, making it the first time it is asked for.
    fn of(
        &mut self,
        colour: u32,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.0
            .entry(colour)
            .or_insert_with(|| {
                materials.add(StandardMaterial {
                    // The wire's colours are sRGB, which is what `srgb_u8` takes. The
                    // character screen's swatches read the same bytes the same way, and
                    // that is the whole of why a shirt is the colour it was chosen to be.
                    base_color: Color::srgb_u8(
                        ((colour >> 16) & 0xFF) as u8,
                        ((colour >> 8) & 0xFF) as u8,
                        (colour & 0xFF) as u8,
                    ),
                    // Cloth and leather, not armour. Nothing in this world is polished.
                    perceptual_roughness: 0.9,
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
struct Wardrobe<'a> {
    visuals: &'a PlayerVisuals,
    palette: &'a mut BodyMaterials,
    materials: &'a mut Assets<StandardMaterial>,
}

impl Wardrobe<'_> {
    /// The mesh and material each part of one appearance is drawn with.
    fn outfit(
        &mut self,
        worn: Appearance,
    ) -> [(BodyPart, Handle<Mesh>, Handle<StandardMaterial>); BodyPart::IN_DRAWING_ORDER.len()]
    {
        let model = worn.hair_model();
        BodyPart::IN_DRAWING_ORDER.map(|part| {
            (
                part,
                self.visuals.mesh(part, model),
                self.palette.of(part.colour(worn), self.materials),
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
struct Dressing<'w> {
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
    fn wardrobe(&mut self) -> Option<Wardrobe<'_>> {
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
/// Bounded in two directions. An entity that leaves for good takes its entry with it, and
/// the server describes it again if it comes back — its own `described` map is dropped
/// per entity for the same reason, so the two sides agree without either being told.
/// An entry that never finds a body is dropped after [`APPEARANCE_GRACE`].
#[derive(Resource, Debug, Default)]
struct Appearances(HashMap<u64, Described>);

/// One cached appearance: what the server said, when it said it, and whether anything
/// has been drawn wearing it yet.
#[derive(Debug, Clone, Copy)]
struct Described {
    appearance: Appearance,
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

/// The vitals the newest accepted snapshot carried, or `None` before one has arrived.
///
/// Replaced wholesale, exactly as [`Inventory`] is. `self_vitals` is present in every
/// snapshot by contract, so there is nothing to merge and nothing to advance: health is
/// never incremented here, damage is never applied here, and the respawn count is never
/// run down from local time. A dropped snapshot costs nothing, because the next one
/// carries the complete answer.
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
/// place to change it. Two questions live here because they are genuinely different, and
/// each system asks the one it means:
///
/// - [`Self::may_aim`] — may the crosshair resolve a voxel. A continuous query, so the
///   frame a mode changes on is allowed to produce an outline.
/// - [`Self::may_act`] — may a request leave this client. Edge-triggered, so the frame a
///   mode changes on belongs to the UI and produces nothing.
///
/// **Neither is authority.** The server owns every outcome an input could ask for and
/// refuses a forged one whatever this answers. What the gate buys is usability and
/// bandwidth: a dead player's controls go quiet instead of firing requests into a
/// refusal, and the client never has to guess which of them the server would have taken.
///
/// The two fields stay private, so `ui/` reads the gate through these methods and only
/// this module and its children — where the bespoke conditions live — reach past them.
#[derive(SystemParam)]
pub struct InputGate<'w> {
    mode: Res<'w, InputMode>,
    vitals: Res<'w, SelfVitals>,
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

    /// Whether aiming, targeting and the outline are live.
    pub fn may_aim(&self) -> bool {
        *self.mode == InputMode::Playing && !self.vitals.dead()
    }

    /// Whether a gameplay request may be originated this frame.
    ///
    /// Stricter than [`Self::may_aim`] by the mode's change flag: a transition and the key
    /// or click that caused it share a frame, and treating that frame as UI-owned is what
    /// keeps clicking *Resume* from also swinging at the block behind the button.
    pub fn may_act(&self) -> bool {
        *self.mode == InputMode::Playing && !self.mode.is_changed() && !self.vitals.dead()
    }
}

/// Marks an entity the snapshots drive, and carries the identity it is drawn for.
#[derive(Component, Debug, Clone, Copy)]
struct Body(u64);

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
/// The local player has none, because it has no body: see [`spawn_body`].
#[derive(Component, Debug, Clone, Copy, PartialEq)]
struct Worn(Appearance);

/// One drawn part of one body — the six children a body has.
#[derive(Component, Debug, Clone, Copy)]
struct BodyVisual(BodyPart);

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

fn create_player_visuals(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(PlayerVisuals {
        shoes: meshes.add(part_mesh(BodyPart::Shoes, ANY_HAIR)),
        trousers: meshes.add(part_mesh(BodyPart::Trousers, ANY_HAIR)),
        shirt: meshes.add(part_mesh(BodyPart::Shirt, ANY_HAIR)),
        skin: meshes.add(part_mesh(BodyPart::Skin, ANY_HAIR)),
        eyes: meshes.add(part_mesh(BodyPart::Eyes, ANY_HAIR)),
        hair: HairModel::ALL.map(|model| (model, meshes.add(part_mesh(BodyPart::Hair, model)))),
    });
}

/// One part of the rig, merged into a single mesh.
///
/// **Authored with its origin at the feet**, which is where the server puts the position
/// it sends, exactly as a mob's parts are. So a body entity's `Transform` is the *feet*
/// position the snapshot carries rather than a centre only this module would know about,
/// the children carry no offset of their own, and the camera can add an eye height to the
/// same number. The capsule this replaces baked the same property in by translating
/// itself up half a body; `player::appearance` measures from the ground instead, so
/// nothing here has to.
///
/// Merged per part rather than per box, because a material is what a part *is*: five
/// parts and a haircut is six draws for a body, where a box apiece would be sixteen.
fn part_mesh(part: BodyPart, model: HairModel) -> Mesh {
    let mut boxes = body_boxes(part, model).iter().map(|cell| {
        let placed = placed_box(part, *cell);
        Mesh::from(Cuboid::from_size(placed.size)).translated_by(placed.centre)
    });

    // Unreachable: every part in the table is drawn from at least one box, and
    // `every_hair_model_is_a_silhouette_of_its_own` is what says so. An empty mesh is the
    // cosmetic failure the rest of this module already prefers to a panic in a renderer.
    let Some(mut merged) = boxes.next() else {
        error!("{part:?} is drawn from no boxes at all");
        return Mesh::from(Cuboid::from_size(Vec3::ZERO));
    };
    merge_all(&mut merged, boxes, "player body");
    merged
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
    mut intent: ResMut<MoveIntent>,
    mut look: ResMut<LookState>,
) {
    // A mode transition and its key or pointer event share a frame. Treat that frame as
    // UI-owned too, so clicking Resume cannot also swing at the block behind the button.
    if *gate.mode != InputMode::Playing || gate.mode.is_changed() {
        set_if_changed(&mut intent, MoveIntent::default());
        return;
    }

    if let Some(pointer) = pointer
        && pointer.delta != Vec2::ZERO
    {
        // Right turns right: looking along -Z, turning towards +X is a *negative* rotation
        // about +Y. Screen y grows downward, so a downward drag has to lower the pitch.
        let next = LookState {
            yaw: look.yaw - pointer.delta.x * LOOK_SENSITIVITY,
            pitch: (look.pitch - pointer.delta.y * LOOK_SENSITIVITY).clamp(-MAX_PITCH, MAX_PITCH),
        };
        // Wrapped here rather than left to grow, so the yaw stays a number a lerp can use:
        // the server wraps what it echoes, and a client whose own copy had drifted a
        // thousand turns away would disagree with every snapshot about which way it faces.
        set_if_changed(&mut look, wrap_look(next));
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

    // Held, not pressed: PlayerInput describes the state of the controls each tick, so what
    // matters is whether the key is down when the frame is sampled.
    let axis = |negative: KeyCode, positive: KeyCode| {
        f32::from(keys.pressed(positive)) - f32::from(keys.pressed(negative))
    };

    let next = MoveIntent {
        x: axis(KeyCode::KeyA, KeyCode::KeyD),
        z: axis(KeyCode::KeyS, KeyCode::KeyW),
        jump: keys.pressed(KeyCode::Space),
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
    mut vitals: ResMut<SelfVitals>,
    mut sky: ResMut<sky::SkyClock>,
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
        if buffer.accept(snapshot, at) {
            // The whole value, never a merge. `set_if_changed` because an unchanged answer
            // is not news — it is what lets the death countdown hold rather than churn the
            // UI that reads it.
            set_if_changed(&mut vitals, SelfVitals(Some(self_vitals)));
            sky.anchor(tick_of_day, at);
        } else {
            // Server ticks are monotonic per session, so this is a duplicate. Debug rather
            // than warn: it costs nothing and means nothing went wrong.
            debug!("a snapshot that was not newer than the newest held was discarded");
        }
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

/// Puts every entity the session can see where the interpolation says it is.
///
/// One entity per identity the server sent, spawned when it first appears and despawned
/// when it stops appearing. The **latest snapshot is the whole truth** about what this
/// session can see: an entity the server has stopped mentioning has left the view distance,
/// and keeping its body would leave a ghost standing where it was last seen.
fn apply_snapshots(
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mut dressing: Dressing<'_>,
    mut appearances: ResMut<Appearances>,
    mut existing: Query<(Entity, &Body, &mut Transform)>,
    mut commands: Commands,
) {
    // Both exist from the first frame after startup. A frame without them is a frame before
    // there is a session, and there is nothing to place a body relative to.
    let (Some(session), Some(mut wardrobe)) = (session, dressing.wardrobe()) else {
        return;
    };

    let now = Instant::now();
    let drawn = buffer.sample(now, tick_interval(session.0.tick_rate));

    // The world is the authority on which bodies exist, rather than a map kept beside it.
    // A map would be a second copy of the same fact and could drift from it — a despawned
    // entity still recorded, or a recorded entity that was never spawned. Scanning a handful
    // of bodies per frame is cheaper than that class of bug.
    let mut placed = HashSet::with_capacity(drawn.len());
    for (entity, body, mut transform) in &mut existing {
        match drawn.iter().find(|(entity_id, _)| *entity_id == body.0) {
            Some((_, state)) => {
                *transform = placement(state);
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
        let worn = described
            .as_ref()
            .map_or(PLACEHOLDER_APPEARANCE, |described| described.appearance);
        if let Some(described) = described {
            described.drawn = true;
        }

        spawn_body(
            &mut commands,
            &mut wardrobe,
            *entity_id,
            session.0.entity_id,
            worn,
            placement(state),
        );
    }

    // **What keeps two caches the size of a view rather than of a session.** An entity in
    // the newest snapshot keeps its entry; one that has left loses it, and the server
    // describes it again if it comes back — `Player.described` on the far side drops its
    // own entry for the same reason, so the two agree without either being told. An entry
    // that has never had a body is the one case a snapshot cannot answer, and it is held
    // for [`APPEARANCE_GRACE`] and no longer.
    let before = appearances.0.len();
    appearances.0.retain(|entity_id, described| {
        drawn.iter().any(|(visible, _)| visible == entity_id)
            || (!described.drawn && now.duration_since(described.at) < APPEARANCE_GRACE)
    });

    // Only when something actually left, because the sweep is a scan of both maps and
    // almost every frame has nothing to sweep.
    if appearances.0.len() != before {
        let live: HashSet<u32> = appearances
            .0
            .values()
            .flat_map(|described| {
                BodyPart::IN_DRAWING_ORDER.map(|part| part.colour(described.appearance))
            })
            .chain(BodyPart::IN_DRAWING_ORDER.map(|part| part.colour(PLACEHOLDER_APPEARANCE)))
            .collect();
        wardrobe.palette.0.retain(|colour, _| live.contains(colour));
    }
}

/// Puts every appearance the net thread decoded into the cache, newest last.
///
/// Runs **before** [`apply_snapshots`], so a body spawned on the frame its appearance
/// arrives is dressed on that frame rather than showing a placeholder for one of them.
/// Whether an entity has been drawn survives an update, because it is a fact about this
/// client and not about the message.
fn ingest_appearances(mut inbox: ResMut<AppearanceInbox>, mut appearances: ResMut<Appearances>) {
    let arrived = inbox.take();
    if arrived.is_empty() {
        return;
    }

    let now = Instant::now();
    for message in arrived {
        let drawn = appearances
            .0
            .get(&message.entity_id)
            .is_some_and(|described| described.drawn);
        appearances.0.insert(
            message.entity_id,
            Described {
                appearance: message.appearance,
                at: now,
                drawn,
            },
        );
    }
}

/// Dresses every body whose appearance has changed since it was drawn.
///
/// **In place, and that is the acceptance criterion**: an entity whose appearance arrives
/// after it does keeps its identity, its transform and its interpolation, and swaps six
/// handles. Despawning and respawning it would restart both and blink the body.
///
/// It is also what makes a *changed* appearance free: the comparison against [`Worn`] is
/// one equality per body per frame, and the loop below runs only for the bodies where it
/// failed.
fn dress_bodies(
    appearances: Res<Appearances>,
    mut dressing: Dressing<'_>,
    mut bodies: Query<(&Body, &mut Worn, &Children)>,
    mut parts: Query<(
        &BodyVisual,
        &mut Mesh3d,
        &mut MeshMaterial3d<StandardMaterial>,
    )>,
) {
    let Some(mut wardrobe) = dressing.wardrobe() else {
        return;
    };

    for (body, mut worn, children) in &mut bodies {
        let Some(described) = appearances.0.get(&body.0) else {
            continue;
        };
        if described.appearance == worn.0 {
            continue;
        }
        worn.0 = described.appearance;

        let outfit = wardrobe.outfit(described.appearance);

        for child in children {
            let Ok((visual, mut mesh, mut material)) = parts.get_mut(*child) else {
                continue;
            };
            let Some((_, shape, colour)) = outfit.iter().find(|(part, _, _)| *part == visual.0)
            else {
                continue;
            };
            // The hair is the one part whose *shape* a player chooses, so it is the one
            // part where a mesh handle can change. The other five swap a colour.
            if mesh.0 != *shape {
                mesh.0 = shape.clone();
            }
            if material.0 != *colour {
                material.0 = colour.clone();
            }
        }
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
/// This session's own player gets **no mesh and no children**. The camera sits at its
/// eyes, so a body there would fill the screen with the inside of the player's own head.
/// A third-person view is what would want one, and that is a camera issue rather than this
/// one — which is also why it carries no [`Worn`]: there is nothing to dress.
///
/// Everybody else gets one child per part, all six under the one transform, and none of
/// them carries an offset: the meshes are authored with their origin at the feet, which is
/// the point the parent already stands on.
fn spawn_body(
    commands: &mut Commands,
    wardrobe: &mut Wardrobe<'_>,
    entity_id: u64,
    local_entity_id: u64,
    worn: Appearance,
    placed: Transform,
) {
    if entity_id == local_entity_id {
        commands.spawn((Body(entity_id), LocalPlayer, placed));
        return;
    }

    let parts = wardrobe.outfit(worn);
    let owner = commands.spawn((Body(entity_id), Worn(worn), placed)).id();
    commands.entity(owner).with_children(|parent| {
        for (part, mesh, material) in parts {
            parent.spawn((
                BodyVisual(part),
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
            ));
        }
    });
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

#[cfg(test)]
mod tests;
