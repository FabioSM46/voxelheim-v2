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
//! | `appearance.rs` | which part of a body each appearance colour covers, and where it sits |

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

use std::collections::HashSet;
use std::time::{Duration, Instant};

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;

pub(crate) use appearance::{BodyPart, parts as body_parts};

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
    LifeState, Outbound, PlayerInput, PlayerVitals, Sent, Session, SnapshotInbox,
    encode_player_input,
};
use constants::{CAPSULE_RADIUS, LOOK_SENSITIVITY, MAX_PITCH, PLAYER_HEIGHT};

/// How far the player has to move before the movement log says so again, in blocks.
///
/// The log line exists because the debug overlay is on a screen, and a screen is what CI,
/// a remote session and an automated end-to-end check do not have — the same reason
/// `world/render.rs` logs when meshing settles. Keyed on distance rather than on time so a
/// player standing still is silent.
const MOVEMENT_LOG_DISTANCE: f32 = 8.0;

/// The colours other players are drawn in, as linear RGB.
///
/// Chosen to sit away from the terrain palette in `world/palette.rs`: a player the same
/// colour as the rock behind them is a player nobody can see. Indexed by entity id, so a
/// given player keeps one colour for as long as the session lasts.
const BODY_COLOURS: [[f32; 3]; 6] = [
    [0.85, 0.25, 0.20], // rust red
    [0.20, 0.55, 0.85], // cold blue
    [0.90, 0.70, 0.20], // amber
    [0.35, 0.75, 0.45], // moss
    [0.70, 0.40, 0.85], // violet
    [0.90, 0.45, 0.65], // heather
];

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
            .init_resource::<SelfVitals>()
            .init_resource::<sky::SkyClock>()
            .init_resource::<PlayerStats>()
            // `init_resource` rather than `insert_resource`, and `NetPlugin` does the same:
            // whichever plugin is built first creates the inbox and the other finds it.
            .init_resource::<SnapshotInbox>()
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
                        drops::animate,
                        mobs::animate,
                        structures::animate,
                        refresh_player_stats,
                    )
                        .chain()
                        .in_set(ApplySnapshots),
                    log_the_players_progress.after(ApplySnapshots),
                    forget_vitals_without_a_session.after(ApplySnapshots),
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

/// The mesh and materials every body is drawn with.
#[derive(Resource, Debug)]
struct PlayerVisuals {
    capsule: Handle<Mesh>,
    colours: Vec<Handle<StandardMaterial>>,
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

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

fn create_player_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // A capsule inscribed in the box the server collides: the radius comes from the
    // footprint's half-width and the cylinder is what is left of the height.
    //
    // Translated up by half the body at build time, so a body entity's `Transform` is the
    // *feet* position the snapshot carries rather than a centre nobody else uses. That
    // matters beyond tidiness: the camera reads the same transform and adds an eye height
    // to it, and a test asserts the transform against the snapshot exactly.
    let capsule = Mesh::from(Capsule3d::new(
        CAPSULE_RADIUS,
        PLAYER_HEIGHT - 2.0 * CAPSULE_RADIUS,
    ))
    .translated_by(Vec3::Y * (PLAYER_HEIGHT / 2.0));

    let colours = BODY_COLOURS
        .iter()
        .map(|[r, g, b]| {
            materials.add(StandardMaterial {
                base_color: Color::linear_rgb(*r, *g, *b),
                // Cloth and leather, not armour. Nothing in this world is polished.
                perceptual_roughness: 0.9,
                ..default()
            })
        })
        .collect();

    commands.insert_resource(PlayerVisuals {
        capsule: meshes.add(capsule),
        colours,
    });
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
    visuals: Option<Res<PlayerVisuals>>,
    mut existing: Query<(Entity, &Body, &mut Transform)>,
    mut commands: Commands,
) {
    // Both exist from the first frame after startup. A frame without them is a frame before
    // there is a session, and there is nothing to place a body relative to.
    let (Some(session), Some(visuals)) = (session, visuals) else {
        return;
    };

    let drawn = buffer.sample(Instant::now(), tick_interval(session.0.tick_rate));

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
        spawn_body(
            &mut commands,
            &visuals,
            *entity_id,
            session.0.entity_id,
            placement(state),
        );
    }
}

/// The transform one interpolated state becomes.
///
/// The translation is the **feet** position the snapshot carries — the capsule mesh is
/// built with its own half-height baked in, so nothing here has to offset it, and the
/// camera can add an eye height to the same number.
fn placement(state: &interpolate::Interpolated) -> Transform {
    Transform {
        translation: state.pos,
        rotation: Quat::from_rotation_y(state.yaw),
        ..default()
    }
}

/// Spawns the entity that draws one of the server's entities.
///
/// This session's own player gets **no mesh**. The camera sits at its eyes, so a capsule
/// there would fill the screen with the inside of the player's own head. A third-person
/// view is what would want one, and that is a camera issue rather than this one.
fn spawn_body(
    commands: &mut Commands,
    visuals: &PlayerVisuals,
    entity_id: u64,
    local_entity_id: u64,
    placed: Transform,
) {
    if entity_id == local_entity_id {
        commands.spawn((Body(entity_id), LocalPlayer, placed));
        return;
    }

    let colour = visuals.colours[(entity_id as usize) % visuals.colours.len()].clone();
    commands.spawn((
        Body(entity_id),
        Mesh3d(visuals.capsule.clone()),
        MeshMaterial3d(colour),
        placed,
    ));
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
