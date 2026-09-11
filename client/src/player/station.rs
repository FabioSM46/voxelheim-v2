//! The crafting station a player has walked up to, and the mode that shows its recipes.
//!
//! **Opening a station sends nothing and decides nothing.** The interact key, pressed at a
//! forge, a campfire or a bench, opens a local panel over the recipes that station makes —
//! exactly as `E` opens the pack. Whether a craft made from that panel succeeds is still
//! `Craft` on the server, which measures its own distance to its own station and refuses in
//! silence; this module only decides which recipes are worth putting in front of a player
//! standing where they are standing.
//!
//! Which structure kinds are stations is not listed here either. It is read off the recipe
//! mirror — a kind is a station when some recipe names it — so a recipe moved to a new bench
//! by the contract makes that bench a station on this side in the same commit, and a tent
//! or a runestone, which no recipe names, is never one.

use bevy::prelude::*;

use super::constants::{MAX_REACH, MOUNTED_HEIGHT, PLAYER_HEIGHT};
use super::crafting::{RECIPES, Recipe};
use super::loot::OriginateInteract;
use super::set_if_changed;
use super::structures::{AimStructures, StationTarget};
use super::{
    ApplyInputMode, ApplySnapshots, InputGate, InputMode, LocalMount, SelfVitals, SnapshotBuffer,
};
use crate::net::{Session, StructureKind};
use crate::settings::{Control, Settings};

/// How close a player must stand to each station to work at it, in blocks, mirrored from
/// `ForgeCraftRadius`, `CampfireCookRadius`, `LeatherBenchCraftRadius`,
/// `ArmourBenchCraftRadius` and `EnchantingTableCraftRadius` in
/// `server/internal/game/craft.go`.
///
/// **This closes a panel and decides nothing else.** No request is gated on it — the server
/// re-measures its own distance to its own station on every craft — so a drift between the
/// copies costs a panel that closes a step early or late, never a craft granted or refused.
const FORGE_CRAFT_RADIUS: f64 = 5.0;
const CAMPFIRE_COOK_RADIUS: f64 = 5.0;
const LEATHER_BENCH_CRAFT_RADIUS: f64 = 5.0;
const ARMOUR_BENCH_CRAFT_RADIUS: f64 = 5.0;
const ENCHANTING_TABLE_CRAFT_RADIUS: f64 = 5.0;

/// The mirrored radius for one kind, or `None` for a kind that is not a station — the same
/// fail-closed shape as the server's `craftRadius`.
const fn station_radius(kind: StructureKind) -> Option<f64> {
    match kind {
        StructureKind::Forge => Some(FORGE_CRAFT_RADIUS),
        StructureKind::Campfire => Some(CAMPFIRE_COOK_RADIUS),
        StructureKind::LeatherBench => Some(LEATHER_BENCH_CRAFT_RADIUS),
        StructureKind::ArmourBench => Some(ARMOUR_BENCH_CRAFT_RADIUS),
        StructureKind::EnchantingTable => Some(ENCHANTING_TABLE_CRAFT_RADIUS),
        StructureKind::Tent | StructureKind::Runestone => None,
    }
}

/// Whether the open station still stands in the newest snapshot, as the same kind, within its
/// radius of this player's body.
///
/// Measured the way `distanceToVoxel` measures it: from the centre of the body's box — the
/// standing position plus half the body's height — to the centre of the anchor voxel, in
/// `f64` because the anchor is a number an untrusted server chose.
///
/// **Absence of evidence keeps the panel open.** No snapshot yet, or one that does not name
/// this player, says nothing about where they stand; closing on it would shut a panel on a
/// stream hiccup. A snapshot that does not name the *structure* is different: the newest
/// snapshot is the existence set, and a station it omits is gone.
fn still_at_station(
    buffer: &SnapshotBuffer,
    player_id: u64,
    open: OpenStation,
    body_height: f32,
) -> bool {
    let Some(latest) = buffer.latest_snapshot() else {
        return true;
    };
    let Some(structure) = latest
        .structures
        .iter()
        .find(|structure| structure.structure_id == open.structure_id)
    else {
        return false;
    };
    let Some(radius) = station_radius(structure.kind).filter(|_| structure.kind == open.kind)
    else {
        return false;
    };
    let Some(me) = latest
        .entities
        .iter()
        .find(|entity| entity.entity_id == player_id)
    else {
        return true;
    };
    let centre = [
        f64::from(me.pos[0]),
        f64::from(me.pos[1]) + f64::from(body_height) / 2.0,
        f64::from(me.pos[2]),
    ];
    let voxel = [
        f64::from(structure.anchor.x) + 0.5,
        f64::from(structure.anchor.y) + 0.5,
        f64::from(structure.anchor.z) + 0.5,
    ];
    let squared: f64 = (0..3)
        .map(|axis| (voxel[axis] - centre[axis]).powi(2))
        .sum();
    squared.sqrt() <= radius
}

/// `ui/mod.rs`'s `set_mode` rule, which `vendor.rs` also restates for the same reason: a mode
/// is written only when it is a different mode, so `InputMode`'s change flag stays honest.
fn set_mode(mode: &mut ResMut<'_, InputMode>, next: InputMode) {
    if **mode != next {
        **mode = next;
    }
}

/// Whether some recipe in the mirror is made at this kind of structure.
pub(super) fn is_craft_station(kind: StructureKind) -> bool {
    RECIPES.iter().any(|recipe| recipe.station == Some(kind))
}

/// Every mirrored recipe made at `station`, in table order — `None` being the recipes made
/// by hand, which are the ones the pack lists.
///
/// One function for both surfaces, so the pack and a station panel partition the table by
/// construction: a recipe cannot appear in both, and cannot appear in neither.
pub fn recipes_made_at(station: Option<StructureKind>) -> impl Iterator<Item = &'static Recipe> {
    RECIPES
        .iter()
        .filter(move |recipe| recipe.station == station)
}

/// What a station is called on screen, in sentence case.
///
/// Exhaustive over every kind rather than over the stations, so a member added to the
/// contract does not compile until somebody has named it. Tent and runestone are named for
/// that reason only; neither ever titles a panel.
pub fn station_title(kind: StructureKind) -> &'static str {
    match kind {
        StructureKind::Forge => "Forge",
        StructureKind::Campfire => "Campfire",
        StructureKind::LeatherBench => "Leather bench",
        StructureKind::ArmourBench => "Armour bench",
        StructureKind::EnchantingTable => "Enchanting table",
        StructureKind::Tent => "Tent",
        StructureKind::Runestone => "Runestone",
    }
}

/// The station the panel is open at, or `None` when no panel is open.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StationWindow {
    current: Option<OpenStation>,
}

impl StationWindow {
    /// Which kind of station the open panel belongs to.
    pub fn station(&self) -> Option<StructureKind> {
        self.current.map(|open| open.kind)
    }

    #[cfg(test)]
    pub(crate) fn at(structure_id: u64, kind: StructureKind) -> Self {
        Self {
            current: Some(OpenStation { structure_id, kind }),
        }
    }
}

/// The interact key chose a station this frame.
///
/// Written by `loot.rs`, which owns the priority the key is resolved through, and applied
/// here, which owns the mode. A message rather than a write because the targeting system
/// reads the mode through [`InputGate`] and cannot also hold it mutably.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OpenStation {
    pub(super) structure_id: u64,
    pub(super) kind: StructureKind,
}

/// The station the world prompt names this frame: one is under the crosshair within reach,
/// the player is playing, and nothing that outranks a station — a lootable corpse — would
/// take the interact key instead.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StationHint(pub Option<StructureKind>);

pub(super) struct StationPlugin;

impl Plugin for StationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StationWindow>()
            .init_resource::<StationHint>()
            .init_resource::<StationTarget>()
            .add_message::<OpenStation>()
            .add_systems(
                Update,
                (
                    // Before the key is resolved, so the press that closes a panel is spent
                    // here — and, more to the point, so the press that *opens* one below is
                    // never read again by this system on the same frame.
                    close_on_interact
                        .after(ApplyInputMode)
                        .before(OriginateInteract),
                    (open_station, close_what_the_player_left)
                        .chain()
                        .after(OriginateInteract)
                        .after(ApplySnapshots),
                    name_the_station_in_reach
                        .after(AimStructures)
                        .after(ApplySnapshots),
                ),
            );
    }
}

/// The interact key closes an open panel, as it opened it.
fn close_on_interact(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    settings: Option<Res<Settings>>,
    mut mode: ResMut<InputMode>,
) {
    if *mode != InputMode::Station {
        return;
    }
    let bindings = settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());
    if keys.is_some_and(|keys| keys.just_pressed(bindings.key(Control::Interact))) {
        set_mode(&mut mode, InputMode::Playing);
    }
}

/// Opens the panel the interact key chose, from play and only from play.
fn open_station(
    mut opens: MessageReader<OpenStation>,
    vitals: Res<SelfVitals>,
    mut window: ResMut<StationWindow>,
    mut mode: ResMut<InputMode>,
) {
    let Some(open) = opens.read().last().copied() else {
        return;
    };
    if vitals.dead() || *mode != InputMode::Playing {
        return;
    }
    window.current = Some(open);
    set_mode(&mut mode, InputMode::Station);
}

/// Takes the panel down when the mode has left it, and takes the mode back on death, a lost
/// session, a walk out of the station's radius, or the station leaving the snapshot.
///
/// `Escape` leaves the mode in `ui/mod.rs`, and the window goes with it here. Death is
/// presentation rather than a rule — the server refuses a craft from a corpse whatever is on
/// screen — and it is repeated here, as the vendor does, so the player plugin keeps it with
/// no UI built. Every write is guarded: an unguarded `ResMut` marks `InputMode` changed on
/// every dead frame, which is what `InputGate::may_act` reads to give a frame to the UI.
fn close_what_the_player_left(
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    buffer: Res<SnapshotBuffer>,
    mount: Option<Res<LocalMount>>,
    mut window: ResMut<StationWindow>,
    mut mode: ResMut<InputMode>,
) {
    let body_height = if mount.is_some_and(|mount| mount.kind().is_some()) {
        MOUNTED_HEIGHT
    } else {
        PLAYER_HEIGHT
    };
    let left = match (session.as_deref(), window.current) {
        (None, _) => true,
        (Some(session), Some(open)) => {
            !still_at_station(&buffer, session.0.entity_id, open, body_height)
        }
        (Some(_), None) => false,
    };
    if (left || vitals.dead()) && *mode == InputMode::Station {
        set_mode(&mut mode, InputMode::Playing);
    }
    if window.current.is_some() && *mode != InputMode::Station {
        window.current = None;
    }
}

/// Names the station the interact key would open, for the world prompt.
///
/// **The one rank above a station is repeated here, and only that one.** The key is resolved
/// corpse, then station, then player, then resident in `loot.rs`; everything below a station
/// loses to it, so the prompt has exactly one thing to ask that the pick does not already
/// answer. `a_corpse_in_reach_silences_the_station_prompt` is what keeps the two in step.
fn name_the_station_in_reach(
    gate: InputGate<'_>,
    session: Option<Res<Session>>,
    buffer: Res<SnapshotBuffer>,
    target: Res<StationTarget>,
    mut hint: ResMut<StationHint>,
) {
    let next = match (target.0, session) {
        (Some(pick), Some(session))
            if gate.mode() == InputMode::Playing
                && !gate.dead()
                && buffer
                    .nearest_accessible_corpse(session.0.entity_id, MAX_REACH)
                    .is_none() =>
        {
            Some(pick.kind)
        }
        _ => None,
    };
    set_if_changed(&mut hint, StationHint(next));
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::ViewMode;
    use super::super::structures::StructurePick;
    use super::*;
    use crate::net::{
        ANY_TOKEN, BlockCoord, EntityState, Facing, MobAction, MobKind, MobState, RecipeId,
        SessionParams, Snapshot, StructureState,
    };

    fn station_at(structure_id: u64, kind: StructureKind, anchor: [i32; 3]) -> StructureState {
        StructureState {
            structure_id,
            kind,
            anchor: BlockCoord {
                x: anchor[0],
                y: anchor[1],
                z: anchor[2],
            },
            facing: Facing::North,
            owner_entity_id: 99,
            lit: true,
        }
    }

    /// Where this player stands so that the body's centre sits `x` blocks along from the
    /// centre of a voxel anchored at the origin, level with it.
    fn standing_at(server_tick: u32, x: f32, structures: Vec<StructureState>) -> Snapshot {
        Snapshot {
            server_tick,
            entities: vec![EntityState {
                entity_id: PLAYER,
                pos: [0.5 + x, 0.5 - PLAYER_HEIGHT / 2.0, 0.5],
                vel: [0.0; 3],
                yaw: 0.0,
            }],
            structures,
            ..Default::default()
        }
    }

    fn open_at(app: &mut App, structure_id: u64, kind: StructureKind) {
        app.world_mut()
            .write_message(OpenStation { structure_id, kind });
        app.update();
    }

    fn see(app: &mut App, snapshot: Snapshot) {
        assert!(
            app.world_mut()
                .resource_mut::<SnapshotBuffer>()
                .accept(snapshot, Instant::now())
        );
        app.update();
    }

    /// Walking out of the mirrored radius closes the panel; standing just inside it does not.
    #[test]
    fn the_panel_closes_when_the_player_walks_out_of_the_station_radius() {
        let forge = || vec![station_at(900, StructureKind::Forge, [0, 0, 0])];
        let mut app = app(standing_at(1, 4.9, forge()));
        open_at(&mut app, 900, StructureKind::Forge);
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Station);

        see(&mut app, standing_at(2, 5.0, forge()));
        assert_eq!(
            *app.world().resource::<InputMode>(),
            InputMode::Station,
            "the radius is inclusive, as `stationWithinLocked`'s `<=` is"
        );

        see(&mut app, standing_at(3, 5.1, forge()));
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        assert_eq!(app.world().resource::<StationWindow>().station(), None);
    }

    /// A station the newest snapshot no longer names is gone, and its panel goes with it.
    #[test]
    fn the_panel_closes_when_the_station_leaves_the_snapshot() {
        let mut app = app(standing_at(
            1,
            1.0,
            vec![station_at(900, StructureKind::LeatherBench, [0, 0, 0])],
        ));
        open_at(&mut app, 900, StructureKind::LeatherBench);
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Station);

        see(&mut app, standing_at(2, 1.0, Vec::new()));
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        assert_eq!(app.world().resource::<StationWindow>().station(), None);
    }

    #[test]
    fn every_station_has_a_radius_and_nothing_else_does() {
        for kind in [
            StructureKind::Forge,
            StructureKind::Campfire,
            StructureKind::LeatherBench,
            StructureKind::ArmourBench,
            StructureKind::EnchantingTable,
            StructureKind::Tent,
            StructureKind::Runestone,
        ] {
            assert_eq!(
                station_radius(kind).is_some(),
                is_craft_station(kind),
                "{kind:?}"
            );
        }
    }

    const PLAYER: u64 = 7;

    fn test_session(entity_id: u64) -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    fn forge_pick() -> StructurePick {
        StructurePick {
            structure_id: 900,
            kind: StructureKind::Forge,
            distance: 2.0,
        }
    }

    fn app(seen: Snapshot) -> App {
        let mut buffer = SnapshotBuffer::default();
        assert!(buffer.accept(seen, Instant::now()));
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(test_session(PLAYER))
            .insert_resource(buffer)
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .add_plugins(StationPlugin);
        app
    }

    fn me() -> Snapshot {
        Snapshot {
            server_tick: 1,
            entities: vec![EntityState {
                entity_id: PLAYER,
                pos: [0.0, 64.0, 0.0],
                vel: [0.0; 3],
                yaw: 0.0,
            }],
            ..Default::default()
        }
    }

    /// The pack and the stations partition the mirror: every recipe is listed on exactly one
    /// surface, hand recipes on the pack and each station's on its own panel.
    #[test]
    fn the_hand_list_and_the_station_lists_partition_the_mirror() {
        let hand: Vec<RecipeId> = recipes_made_at(None).map(|recipe| recipe.id).collect();
        let expected: Vec<RecipeId> = RECIPES
            .iter()
            .filter(|recipe| recipe.station.is_none())
            .map(|recipe| recipe.id)
            .collect();
        assert_eq!(hand, expected);
        assert!(hand.contains(&RecipeId::Forge) && !hand.contains(&RecipeId::IronSword));

        let mut seen: Vec<RecipeId> = hand;
        for kind in [
            StructureKind::Forge,
            StructureKind::Campfire,
            StructureKind::LeatherBench,
            StructureKind::ArmourBench,
            StructureKind::EnchantingTable,
        ] {
            assert!(is_craft_station(kind), "{kind:?} makes nothing");
            for recipe in recipes_made_at(Some(kind)) {
                assert_eq!(recipe.station, Some(kind));
                assert!(
                    !seen.contains(&recipe.id),
                    "{:?} is listed twice",
                    recipe.id
                );
                seen.push(recipe.id);
            }
        }
        assert_eq!(seen.len(), RECIPES.len(), "a recipe is listed nowhere");

        for kind in [StructureKind::Tent, StructureKind::Runestone] {
            assert!(!is_craft_station(kind), "{kind:?} became a station");
        }
    }

    #[test]
    fn a_station_title_is_ascii_and_names_the_bench() {
        assert_eq!(station_title(StructureKind::LeatherBench), "Leather bench");
        for kind in [
            StructureKind::Forge,
            StructureKind::Campfire,
            StructureKind::LeatherBench,
            StructureKind::ArmourBench,
            StructureKind::EnchantingTable,
        ] {
            // Bevy's default font is a 95-glyph ASCII subset; see `ui/loot.rs`.
            assert!(station_title(kind).is_ascii());
        }
    }

    /// The chosen station opens the panel from play, the key closes it again, and death takes
    /// it — each without a single frame on the wire, because there is nothing to send.
    #[test]
    fn a_chosen_station_opens_from_play_and_closes_on_the_key_and_on_death() {
        let mut app = app(standing_at(
            1,
            1.0,
            vec![
                station_at(900, StructureKind::LeatherBench, [0, 0, 0]),
                station_at(901, StructureKind::Forge, [0, 0, 1]),
            ],
        ));
        app.world_mut().write_message(OpenStation {
            structure_id: 900,
            kind: StructureKind::LeatherBench,
        });
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Station);
        assert_eq!(
            app.world().resource::<StationWindow>().station(),
            Some(StructureKind::LeatherBench)
        );

        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyF);
        app.insert_resource(keys);
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        assert_eq!(app.world().resource::<StationWindow>().station(), None);

        app.insert_resource(ButtonInput::<KeyCode>::default());
        app.world_mut().write_message(OpenStation {
            structure_id: 901,
            kind: StructureKind::Forge,
        });
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Station);
        app.insert_resource(SelfVitals::from_server(crate::net::PlayerVitals {
            health: 0,
            life_state: crate::net::LifeState::Dead,
            ..crate::net::PlayerVitals::unharmed()
        }));
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        assert_eq!(app.world().resource::<StationWindow>().station(), None);
    }

    /// A station is never opened over another screen: the key that chose it was pressed in
    /// play, and a message that outlived the mode it was written in belongs to nothing.
    #[test]
    fn a_chosen_station_does_not_open_over_another_screen() {
        let mut app = app(me());
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.world_mut().write_message(OpenStation {
            structure_id: 900,
            kind: StructureKind::Forge,
        });
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Inventory);
        assert_eq!(app.world().resource::<StationWindow>().station(), None);
    }

    #[test]
    fn the_prompt_names_the_station_in_reach_only_while_playing() {
        let mut app = app(me());
        app.insert_resource(StationTarget(Some(forge_pick())));
        app.update();
        assert_eq!(
            app.world().resource::<StationHint>().0,
            Some(StructureKind::Forge)
        );

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();
        assert_eq!(app.world().resource::<StationHint>().0, None);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.insert_resource(StationTarget(None));
        app.update();
        assert_eq!(app.world().resource::<StationHint>().0, None);
    }

    /// The interact key takes a corpse before a station, so the prompt must not name the
    /// station while one is in reach — it would be teaching a press that does something else.
    #[test]
    fn a_corpse_in_reach_silences_the_station_prompt() {
        let mut seen = me();
        seen.mobs = vec![MobState {
            entity_id: 40,
            kind: MobKind::Draugr,
            pos: [2.0, 64.0, 0.0],
            vel: [0.0; 3],
            yaw: 0.0,
            health: 0,
            max_health: 60,
            action: MobAction::Corpse,
            target_entity_id: 0,
        }];
        seen.accessible_loot_corpses = vec![40];
        let mut app = app(seen);
        app.insert_resource(StationTarget(Some(forge_pick())));
        app.update();
        assert_eq!(app.world().resource::<StationHint>().0, None);
    }
}
