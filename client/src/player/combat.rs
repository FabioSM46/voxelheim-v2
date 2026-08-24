//! Which intent the left button means, and the swing it sends.
//!
//! **This routing is not authority.** A modified client can emit either frame, and the
//! server validates the slot and refuses the wrong one — legacy PR 95 checks that the named slot
//! still holds a non-broken blade before a swing resolves. What this module decides is
//! only which intent an *honest* UI should send when the player clicks, so that one click
//! never asks for two different things.
//!
//! Nothing here judges range, cone, cooldown, target, damage or death. Every one of those
//! is the server's answer and arrives as the next snapshot.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::crafting;
use super::inventory::{Inventory, SelectedSlot};
use super::{ApplySnapshots, InputCadence, InputGate, ViewMode};
use crate::net::{AttackRequest, Outbound, Sent, encode_attack_request};

/// The button that swings, and the same one that mines. Which of the two it means is
/// what [`blade_in_hand`] decides.
const SWING_BUTTON: MouseButton = MouseButton::Left;

/// Item id 7, the rusty sword, as `server/internal/game/items.go` appends it.
///
/// Presentation and routing only. It cannot make another item attack-capable and it
/// cannot make this one legal: the server reads its own registry, and a swing naming a
/// slot of stone is refused there whatever this constant says.
pub(super) const ITEM_RUSTY_SWORD: u16 = 7;

/// Every item id this client routes the left button to a swing for.
///
/// **A table rather than a comparison, and that is the whole of the change.** This used to
/// be `item_id == ITEM_RUSTY_SWORD` — one weapon's name spelled inside the routing — which
/// is exactly what `armedWithSwordLocked` stopped doing on the server when it began reading
/// `meleeDamage` out of the item registry instead. A third blade is one more line here, as
/// it is one more registry entry there, and neither is an edit to the predicate.
///
/// It stays this client's own opinion and it still decides nothing: the server re-reads its
/// own registry for every swing, so a wrong entry costs a request that is refused and can
/// never grant a blow. The failure that actually cost something was the other direction —
/// an item the server would have honoured and this list omitted, which is what the iron
/// sword silently was: drawn as a blade, worth 40 damage server-side, and never asked for.
///
/// **Deliberately one list and not a second registry.** legacy PR 128 collapses every per-item fact
/// this client holds — display name, held shape, swatch — into a single table. *The left
/// button swings this* is one more fact of that kind, so this becomes a column of that table
/// and [`item_is_a_blade`] its accessor, with no call site and no test changed.
const BLADES: &[u16] = &[ITEM_RUSTY_SWORD, crafting::ITEM_IRON_SWORD];

/// Whether this client routes the left button to a swing for one item id.
///
/// Split from [`blade_in_hand`] because the two questions are not the same one: this asks
/// about an *item*, and that asks about the *stack in the selected slot*, which also has to
/// be there and not worn through. The split is what lets the sweep in the tests below hold
/// this against `super::hands`'s idea of what a blade looks like item by item, with no stack
/// to build and no durability to choose.
pub(super) fn item_is_a_blade(item_id: u16) -> bool {
    BLADES.contains(&item_id)
}

pub(super) struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        // `PlayerCameraPlugin` owns it in the game; here too, so `InputGate` — which
        // reads it — resolves when this module is built on its own.
        app.init_resource::<ViewMode>();
        app.add_message::<SwingSent>().add_systems(
            Update,
            send_attacks
                .in_set(ApplyCombatInput)
                // After the structure pick, because a press on this player's own camp is
                // a removal rather than a swing and the pick is what says so.
                .after(super::structures::AimStructures)
                // After the snapshots for the reason every other input system is: the gate
                // it reads is published there, and a frame stale means a click landing
                // after the server said the player was dead.
                .after(ApplySnapshots)
                // After the tick-paced input, so the aim frame carrying this tick reaches
                // the server before the swing that names it. The server resolves the swing
                // against the aim it last accepted, and an attack that arrived first would
                // be judged against the previous frame's facing.
                .after(super::send_player_input),
        );
    }
}

/// Marks the system that turns a click into a swing, so a later module can order against
/// it without naming the function.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApplyCombatInput;

/// One swing left this client. Cosmetic feedback reads it; nothing else does.
///
/// Sent whether the blow later hits or misses, because this client does not know which
/// and will not find out except by watching the draugr's health in a later snapshot.
///
/// **A unit struct, and it stays one.** `super::hands` draws three different arcs and picks
/// between them itself, from a cursor that never leaves that module (#174) — so which shape
/// plays is decided *downstream* of this message rather than carried by it, and there is no
/// direction in which it could travel back. That is what keeps the picture from deciding
/// anything: the `AttackRequest` below names a slot and a tick and has nowhere to put an
/// animation, so three consecutive swings ask the server for exactly the same thing.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SwingSent;

/// Whether the selected slot presents a blade this client will swing with.
///
/// The one predicate, read by the sender below and by the mining path in
/// [`super::target`], which is what makes the two mutually exclusive rather than merely
/// unlikely to overlap. A blade worn through is not one: the server refuses a swing from
/// it, so an honest UI mines instead of asking for something it knows will be declined.
///
/// **Worn through means zero under a non-zero maximum**, which is the pair
/// `armedWithSwordLocked` reads and not the current value on its own. `max_durability > 0`
/// is already this client's answer to *does this wear out* — `super::inventory`'s
/// `repair_request` asks it in exactly that shape — and a weapon that never wears out would
/// carry `(0, 0)` like every resource does. Reading the current value alone would call such
/// a weapon permanently broken the day somebody registered one, which is a courtesy refusing
/// a swing the server would have granted: the one direction a courtesy must never fail in.
pub(super) fn blade_in_hand(inventory: &Inventory, selected: &SelectedSlot) -> bool {
    inventory.slot(selected.0).is_some_and(|stack| {
        item_is_a_blade(stack.item_id)
            && stack.count > 0
            && (stack.max_durability == 0 || stack.durability > 0)
    })
}

/// The selected slot and what it holds, as the input systems ask about it.
///
/// A bundle rather than two parameters, because both systems that route the left button
/// need the pair and `send_block_edits` was already at the argument bound. Bundling also
/// puts the shared question — is there a blade in hand — on one type instead of leaving
/// two call sites to remember to ask it the same way.
#[derive(SystemParam)]
pub(super) struct HeldItem<'w> {
    inventory: Res<'w, Inventory>,
    selected: Res<'w, SelectedSlot>,
}

impl HeldItem<'_> {
    /// Which authoritative slot the hotbar has selected.
    pub(super) fn slot(&self) -> u8 {
        self.selected.0
    }

    /// Whether that slot presents a blade this client will swing with.
    pub(super) fn blade(&self) -> bool {
        blade_in_hand(&self.inventory, &self.selected)
    }

    /// Which structure that slot would plant, if it plants one.
    ///
    /// Read by the placement sender in [`super::structures`] and by the block-edit path in
    /// [`super::target`], for the reason [`Self::blade`] is read by two sites: one press
    /// must never ask for a voxel and a building at once, and asking the same function is
    /// what makes that structural.
    pub(super) fn structure(&self) -> Option<crate::net::StructureKind> {
        super::structures::structure_in_hand(
            self.inventory
                .slot(self.selected.0)
                .filter(|stack| stack.count > 0)
                .map(|stack| stack.item_id),
        )
    }
}

/// Sends exactly one `AttackRequest` per press, while a blade is selected.
///
/// `just_pressed`, never `pressed`: a swing is an event and the server refuses a second
/// one inside its cooldown anyway, so holding the button down would only fill the
/// outbound queue with frames that are declined on arrival.
fn send_attacks(
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    gate: InputGate<'_>,
    held: HeldItem<'_>,
    cadence: Res<InputCadence>,
    outbound: Option<ResMut<Outbound>>,
    structure: Res<super::structures::StructureTarget>,
    mut swings: MessageWriter<SwingSent>,
) {
    if !gate.may_act() {
        return;
    }
    let Some(buttons) = buttons else {
        return;
    };
    if !buttons.just_pressed(SWING_BUTTON) {
        return;
    }
    // One of this player's own structures under the crosshair takes this button, exactly
    // as it takes the mining half in [`super::target`]. Swinging at your own tent and
    // asking for it back are two different requests, and a press sends at most one.
    if structure.0.is_some() {
        return;
    }
    if !held.blade() {
        return;
    }

    let Some(mut outbound) = outbound else {
        return;
    };
    let request = AttackRequest {
        slot: held.slot(),
        // The counter `PlayerInput`, placement and mining all share, so the server can
        // order a swing against the aim frame that carries the same number.
        client_tick: cadence.client_tick,
    };
    // The animation is feedback for a frame that *left*, which is what `SwingSent` says
    // it is and what the acceptance criterion asks for — "every sent swing". A dropped
    // frame is not a sent swing, and animating one would tell the player they attacked
    // when nothing was asked of the server. It is still feedback for the *asking* rather
    // than for a hit: whether the blow lands is not known here and never will be.
    match outbound.send(encode_attack_request(&request)) {
        Sent::Queued => {
            swings.write(SwingSent);
        }
        Sent::Dropped => {
            warn!(
                "the outbound queue was full; a swing from slot {} never reached the server",
                request.slot
            );
        }
        // The session is ending. There is nowhere to send and nothing to celebrate.
        Sent::Closed => {}
    }
}

#[cfg(test)]
mod tests {
    //! No window, no display and no GPU. What is asserted is the bytes that left, because
    //! the frame is what the server acts on.

    use std::sync::mpsc::Receiver;

    use bevy::asset::AssetPlugin;
    use bevy::input::ButtonState;
    use bevy::input::InputPlugin;
    use bevy::input::mouse::MouseButtonInput;

    use super::*;
    use crate::net::{
        InventoryInbox, InventoryStack, InventoryState, PlayerVitals, Session, SessionParams,
    };
    use crate::player::crafting::{ITEM_IRON_SWORD, ITEM_SHARPENING_STONE};
    use crate::player::items::{ITEM_LOG, ITEM_RAW_COAL, ITEM_RAW_IRON, ITEM_STONE, ItemShape};
    use crate::player::structures::{ITEM_FORGE, ITEM_TENT};
    use crate::player::{InputMode, PlayerPlugin, SelfVitals};
    use crate::wire::voxelheim::net as fb;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.5, 64.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 36,
            hotbar_slots: 9,
            equipment_slots: 3,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    /// An app that can click and somewhere for the frames to go.
    ///
    /// The queue is deeper than any of these tests needs, so a full one can never be what
    /// makes a request go missing.
    fn clicking_app(slot_zero: InventoryStack) -> (App, Receiver<Vec<u8>>) {
        let mut app = App::new();
        let (outbound, sent) = Outbound::to_a_test(64);
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), InputPlugin))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(outbound)
            .add_plugins(PlayerPlugin);

        deliver(&mut app, slot_zero);
        app.update();
        drain(&sent);
        (app, sent)
    }

    /// Replaces the pack wholesale, as one more complete `InventoryState` from the server.
    ///
    /// Whole rather than edited, because that is the only kind of inventory this client
    /// has: there is no `set_slot` here to reach for.
    fn deliver(app: &mut App, slot_zero: InventoryStack) {
        let mut stacks = vec![InventoryStack::default(); 36];
        stacks[0] = slot_zero;
        app.world_mut()
            .resource_mut::<InventoryInbox>()
            .push(InventoryState { stacks });
    }

    /// One of an item, which is every stack these tests need except a blade.
    fn one(item_id: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count: 1,
            ..Default::default()
        }
    }

    /// One blade at full health, whichever blade it is.
    fn blade_of(item_id: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count: 1,
            durability: 100,
            max_durability: 100,
        }
    }

    /// The starter blade, which the tests about the gate rather than the item still need
    /// exactly one of.
    fn blade() -> InventoryStack {
        blade_of(ITEM_RUSTY_SWORD)
    }

    /// Both blades, named.
    ///
    /// The invariant this issue exists for is that the routing cannot tell them apart, so
    /// the tests that are about routing run over the pair. One test each would let the two
    /// drift into asserting different things, which is how the client came to draw a blade
    /// it would not swing.
    fn blades() -> [(&'static str, InventoryStack); 2] {
        [
            ("the rusty sword", blade_of(ITEM_RUSTY_SWORD)),
            ("the iron sword", blade_of(ITEM_IRON_SWORD)),
        ]
    }

    /// Writing the message rather than poking the resource: `mouse_button_input_system`
    /// clears `just_pressed` at the start of every frame, so a press written directly
    /// would arrive at Update already forgotten.
    fn click(app: &mut App) {
        app.world_mut().write_message(MouseButtonInput {
            button: SWING_BUTTON,
            state: ButtonState::Pressed,
            window: Entity::PLACEHOLDER,
        });
    }

    /// Letting go, so the next [`click`] is another `just_pressed` rather than nothing.
    ///
    /// `ButtonInput::press` on a button that is already down sets no `just_pressed` flag —
    /// which is exactly the behaviour `holding_the_button_does_not_repeat_the_swing` relies
    /// on — so a test about *consecutive* presses has to release between them or it is a
    /// test about one press with extra frames in it.
    fn release(app: &mut App) {
        app.world_mut().write_message(MouseButtonInput {
            button: SWING_BUTTON,
            state: ButtonState::Released,
            window: Entity::PLACEHOLDER,
        });
    }

    fn drain(sent: &Receiver<Vec<u8>>) {
        while sent.try_recv().is_ok() {}
    }

    /// Every attack request waiting on the queue, read out of the encoded bytes.
    ///
    /// Filtered, because this queue also carries the tick-paced input stream.
    fn attacks(sent: &Receiver<Vec<u8>>) -> Vec<(u8, u32)> {
        let mut found = Vec::new();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            if let Some(request) = envelope.payload_as_attack_request() {
                found.push((request.slot(), request.client_tick()));
            }
        }
        found
    }

    /// One press, one swing — from either blade, and the same swing from both.
    ///
    /// The iron sword's half is the whole of this issue: the server has granted it 40
    /// damage since the crafting work landed, and this client was the only thing that never
    /// asked. Nothing about the request distinguishes the two, because nothing may: which
    /// blade a slot holds is the server's to read out of its own registry.
    #[test]
    fn one_click_with_a_blade_sends_exactly_one_swing() {
        for (name, stack) in blades() {
            let (mut app, sent) = clicking_app(stack);
            click(&mut app);
            app.update();

            let found = attacks(&sent);
            assert_eq!(
                found.len(),
                1,
                "{name}: one click sent {} swings",
                found.len()
            );
            assert_eq!(found[0].0, 0, "{name}: the swing named the wrong slot");
        }
    }

    /// The swing carries the counter `PlayerInput` uses, so the server can order the aim
    /// frame ahead of the blow that names it — and carries it for both blades, because the
    /// cadence gate is the same gate.
    #[test]
    fn the_swing_carries_the_shared_client_tick() {
        for (name, stack) in blades() {
            let (mut app, sent) = clicking_app(stack);
            click(&mut app);
            app.update();

            let tick = app.world().resource::<InputCadence>().client_tick;
            assert_eq!(attacks(&sent), vec![(0, tick)], "{name}");
        }
    }

    /// A swing is an event. Holding the button asks once, and the server's cooldown would
    /// refuse the rest anyway.
    #[test]
    fn holding_the_button_does_not_repeat_the_swing() {
        let (mut app, sent) = clicking_app(blade());
        click(&mut app);
        app.update();
        drain(&sent);

        for _ in 0..5 {
            app.update();
        }
        assert!(
            attacks(&sent).is_empty(),
            "holding the button kept sending swings"
        );
    }

    /// Both worn-through blades are in here, and that is the point of listing them.
    ///
    /// The server refuses a swing from a blade at zero durability, so a client that asked
    /// anyway would be firing intent into a certain refusal — and the player would see a
    /// press that did nothing at all, where mining is what the press should have meant.
    #[test]
    fn nothing_but_a_working_blade_swings() {
        for (name, stack) in [
            ("an empty slot", InventoryStack::default()),
            (
                "a stack of stone",
                InventoryStack {
                    item_id: ITEM_STONE,
                    count: 10,
                    ..Default::default()
                },
            ),
            ("a sharpening stone", one(ITEM_SHARPENING_STONE)),
            (
                "a rusty blade worn through",
                InventoryStack {
                    durability: 0,
                    ..blade_of(ITEM_RUSTY_SWORD)
                },
            ),
            (
                "an iron blade worn through",
                InventoryStack {
                    durability: 0,
                    ..blade_of(ITEM_IRON_SWORD)
                },
            ),
        ] {
            let (mut app, sent) = clicking_app(stack);
            click(&mut app);
            app.update();
            assert!(attacks(&sent).is_empty(), "{name} sent a swing");
        }
    }

    /// The gate legacy PR 96 added, and the reason a click that closed a menu cannot swing on the
    /// frame play resumes.
    #[test]
    fn a_ui_mode_or_a_death_suppresses_the_swing() {
        for (name, prepare) in [
            (
                "the pack is open",
                (|app: &mut App| {
                    *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
                }) as fn(&mut App),
            ),
            ("the menu is open", |app: &mut App| {
                *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
            }),
            ("the server says dead", |app: &mut App| {
                *app.world_mut().resource_mut::<SelfVitals>() =
                    SelfVitals::from_server(PlayerVitals {
                        health: 0,
                        max_health: 100,
                        hunger: 50,
                        max_hunger: 100,
                        level: 1,
                        experience: 0,
                        experience_to_next: 50,
                        life_state: crate::net::LifeState::Dead,
                        respawn_ticks: 40,
                        invulnerable: false,
                    });
            }),
        ] {
            let (mut app, sent) = clicking_app(blade());
            prepare(&mut app);
            click(&mut app);
            app.update();
            assert!(attacks(&sent).is_empty(), "{name} still sent a swing");
        }
    }

    /// A swing that could not leave animates nothing.
    ///
    /// The outbound queue is bounded and lossy by design — what waits there is input, and
    /// a producer that cannot block has to be able to drop. Playing the arc anyway would
    /// tell the player they attacked when nothing was asked of the server, which is the
    /// one thing cosmetic feedback must never do.
    #[test]
    fn a_dropped_swing_is_not_animated() {
        // One slot, and the tick-paced input stream fills it before the click lands.
        let mut app = App::new();
        let (outbound, sent) = Outbound::to_a_test(1);
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), InputPlugin))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(outbound)
            .add_plugins(PlayerPlugin);

        let mut stacks = vec![InventoryStack::default(); 36];
        stacks[0] = blade();
        app.world_mut()
            .resource_mut::<InventoryInbox>()
            .push(InventoryState { stacks });

        app.update();

        // Filled explicitly rather than by waiting for the input stream to do it: the
        // input cadence is time-paced, so an app that runs two frames in a microsecond
        // may never send one — which is how the first version of this test found the
        // queue empty and proved nothing.
        app.world_mut()
            .resource_mut::<Outbound>()
            .send(vec![0u8; 4]);

        click(&mut app);
        app.update();

        assert!(
            attacks(&sent).is_empty(),
            "the queue was supposed to be full, so this test proves nothing"
        );
        assert_eq!(
            app.world().resource::<Messages<SwingSent>>().len(),
            0,
            "a swing that never left the client still animated"
        );
    }

    /// The predicate both routing sites read, over both blades and over everything else
    /// this build knows how to carry.
    ///
    /// Testing it directly is what makes "one click never sends both" a property of one
    /// function rather than of two that agree today. Testing the two blades in one table is
    /// what makes "the iron sword behaves exactly like the rusty one" a property rather
    /// than a coincidence between tests written months apart.
    #[test]
    fn the_blade_predicate_is_exact() {
        let selected = SelectedSlot(0);

        for (blade, item_id) in [
            ("the rusty sword", ITEM_RUSTY_SWORD),
            ("the iron sword", ITEM_IRON_SWORD),
        ] {
            for (wear, durability, max_durability, want) in [
                ("fresh", 100, 100, true),
                ("one hit from worn through", 1, 100, true),
                ("worn through", 0, 100, false),
                // A weapon the server registered with no maximum at all would arrive as
                // `(0, 0)`, which is what every resource carries and what `net::codec`
                // documents as "does not wear out". Reading the current value alone would
                // call it broken on arrival and refuse a swing the server would grant.
                ("registered as never wearing out", 0, 0, true),
            ] {
                let inventory = Inventory::from_stacks(vec![InventoryStack {
                    item_id,
                    count: 1,
                    durability,
                    max_durability,
                }]);
                assert_eq!(
                    blade_in_hand(&inventory, &selected),
                    want,
                    "{blade}, {wear}"
                );
            }
        }

        // And nothing else swings. An item that gained a `meleeDamage` on the server would
        // have to gain an entry in `BLADES` before it moved out of this list, which is the
        // whole reason the predicate is a table.
        for (name, stack) in [
            ("an empty slot", InventoryStack::default()),
            ("stone", one(ITEM_STONE)),
            ("a log", one(ITEM_LOG)),
            ("raw coal", one(ITEM_RAW_COAL)),
            ("raw iron", one(ITEM_RAW_IRON)),
            ("a tent", one(ITEM_TENT)),
            ("a forge", one(ITEM_FORGE)),
            ("a sharpening stone", one(ITEM_SHARPENING_STONE)),
            ("an id from a newer contract", one(u16::MAX)),
            (
                "a blade slot with nothing left in it",
                InventoryStack {
                    count: 0,
                    ..blade_of(ITEM_IRON_SWORD)
                },
            ),
        ] {
            let inventory = Inventory::from_stacks(vec![stack]);
            assert!(!blade_in_hand(&inventory, &selected), "{name} swings");
        }
    }

    /// **Three swings in a row ask the server for exactly the same thing.**
    ///
    /// The hand draws a different arc for each of them — an overhead cut, a lateral slash
    /// and a thrust, rotating so no two consecutive presses repeat (#174) — and not one of
    /// those shapes reaches this module. `AttackRequest` carries a slot and the shared
    /// counter, and this reads both: the slot is identical across the three, and the tick is
    /// the only thing that moves, because time passed rather than because a picture changed.
    ///
    /// It is the sending half of *a picture decides nothing*. The drawing half is pinned in
    /// `super::hands`, where the cursor lives; what could not be checked there is that
    /// nothing leaked into the frame, and a frame is the only thing the server acts on.
    #[test]
    fn consecutive_swings_ask_for_the_same_thing() {
        let (mut app, sent) = clicking_app(blade());

        let mut asked = Vec::new();
        for press in 0..3 {
            click(&mut app);
            app.update();
            release(&mut app);
            app.update();
            let found = attacks(&sent);
            assert_eq!(found.len(), 1, "press {press} sent {} swings", found.len());
            asked.push(found[0]);
        }

        let slots: Vec<u8> = asked.iter().map(|(slot, _)| *slot).collect();
        assert_eq!(
            slots,
            vec![0, 0, 0],
            "the three swings named different slots: {asked:?}"
        );

        // And the presses really were three, rather than one that the queue echoed: the
        // shared counter moves with the tick-paced input stream, so the ticks are allowed to
        // differ and are the only field that is.
        assert_eq!(asked.len(), 3);
    }

    /// Every item the hand draws as a blade also routes the left button to a swing.
    ///
    /// The contradiction this issue closed, pinned so it cannot come back. `super::hands`
    /// drew the iron sword as a `Blade` while this module said it was not one, so the hand
    /// showed a weapon that mined — and the comment beside that arm named the gap on
    /// purpose rather than hiding it. Two opinions in two modules is the shape of the bug,
    /// and it survives until legacy PR 128 folds them into one table; this is what holds them
    /// together in the meantime.
    ///
    /// Read through the **shape the hand actually builds**, rather than reading the item
    /// table directly. The view model now keeps one stable mesh handle and rebuilds that
    /// asset in place, so handle identity deliberately says nothing about shape; the
    /// test-only accessor in `super::hands` reaches the same `selected_appearance` route the
    /// running client does.
    ///
    /// Swept over a **range** rather than over the ids that exist today: a hand-written
    /// list would be a third copy of the item table, and the entry it lost would be the
    /// new one.
    #[test]
    fn every_item_the_hand_draws_as_a_blade_also_swings() {
        // Comfortably past the highest id `server/internal/game/items.go` registers, so an
        // item appended there without a thought for this test is still swept.
        const HIGHEST_SWEPT_ID: u16 = 64;

        let mut drawn_as_blades = Vec::new();
        for item_id in 1..=HIGHEST_SWEPT_ID {
            if super::super::hands::drawn_item_shape(item_id) == ItemShape::Blade {
                drawn_as_blades.push(item_id);
                assert!(
                    item_is_a_blade(item_id),
                    "item {item_id} is drawn as a blade and the left button still mines with it"
                );
            }
        }

        // The other direction, and the proof this sweep is not passing vacuously: a run
        // that matched nothing at all would satisfy the loop above perfectly.
        for item_id in [ITEM_RUSTY_SWORD, ITEM_IRON_SWORD] {
            assert!(
                drawn_as_blades.contains(&item_id),
                "item {item_id} swings and the hand does not draw it as a blade"
            );
        }
    }
}
