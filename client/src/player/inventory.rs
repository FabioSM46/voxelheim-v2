//! The local mirror of the inventory the server owns.
//!
//! There are deliberately no `add` or `consume` methods here. An [`InventoryState`]
//! replaces the whole resource, and nothing a click requests changes a count. Selection
//! is local input, like the camera direction: it chooses the slot index carried by the
//! next request, while the server decides whether that slot exists and may be spent.
//!
//! **One cell, four possible intents, and the choice between them is routing rather than
//! authority.** A consume press on known food asks to eat one; a picked sharpening stone
//! dropped on a slot that wears out asks for a mend; a shift-click asks for the stack to be
//! put on the ground; every other pair asks for the move it has always asked for. Which item
//! is edible or a legal kit, how much hunger or wear comes back, whether a stack may be let
//! go of at all and what appears on the ground are the server's answers. No branch moves a
//! count, durability or vital here.

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;

use super::crafting::{
    ITEM_COOKED_MEAT, ITEM_IRON_CUIRASS, ITEM_IRON_GREAVES, ITEM_IRON_HELM, ITEM_LEATHER_CAP,
    ITEM_LEATHER_JERKIN, ITEM_LEATHER_LEGGINGS, ITEM_LEATHER_PATCH, ITEM_SHARPENING_STONE,
    ITEM_WOODEN_SHIELD,
};
use super::items::ITEM_RAW_MEAT;
use super::{
    ApplyInputMode, ApplySnapshots, InputCadence, InputGate, InputMode, SelfVitals, ViewMode,
    set_if_changed,
};
use crate::net::{
    ConsumeRequest, DropItemRequest, InventoryInbox, InventoryMoveRequest, InventoryStack,
    Outbound, RepairRequest, Sent, Session, encode_consume_request, encode_drop_item_request,
    encode_inventory_move_request, encode_repair_request,
};
use crate::settings::{Control, Settings};

/// Number keys available to the minimal hotbar.
const HOTBAR_KEYS: [KeyCode; 9] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
];

/// The last complete inventory the server sent.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Inventory {
    stacks: Vec<InventoryStack>,
    silver: u32,
}

impl Inventory {
    /// Every authoritative slot, exactly as the latest message carried it.
    #[cfg(test)]
    pub fn stacks(&self) -> &[InventoryStack] {
        &self.stacks
    }

    /// The server's total count for one item id across every slot.
    ///
    /// Summed across slots because the server's own `consume` spends across slots. Read by
    /// the crafting mirror in [`super::crafting`] to gray out a row nobody can afford — a
    /// courtesy computed from the last complete state the server sent, and never a verdict
    /// about the next one. Reading is all it does: there is still no `add` and no
    /// `consume` on this type.
    pub fn count(&self, item_id: u16) -> u32 {
        self.stacks
            .iter()
            .filter(|stack| stack.item_id == item_id)
            .map(|stack| u32::from(stack.count))
            .sum()
    }

    /// The server-owned purse from the latest complete state.
    ///
    /// Currency is not reconstructed from slots: the server sends this total beside the
    /// slot vector, and every client surface reads that same answer.
    pub fn silver(&self) -> u32 {
        self.silver
    }

    /// One authoritative slot, when the server has sent it.
    pub fn slot(&self, slot: u8) -> Option<InventoryStack> {
        self.stacks.get(usize::from(slot)).copied()
    }

    /// A server-sent state without a socket, for modules that render this resource in
    /// headless tests.
    #[cfg(test)]
    pub(crate) fn from_stacks(stacks: Vec<InventoryStack>) -> Self {
        Self { stacks, silver: 0 }
    }

    /// A complete server-sent state without a socket, including its purse.
    #[cfg(test)]
    pub(crate) fn from_state(stacks: Vec<InventoryStack>, silver: u32) -> Self {
        Self { stacks, silver }
    }
}

/// The authoritative slot a place request will ask the server to spend.
///
/// Slot zero is selected initially even when empty. Selection is input, not an
/// inventory outcome, so a server-sent state never changes it.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SelectedSlot(pub u8);

/// What a press on an inventory cell was asking for.
///
/// Two variants are amount shapes and two are one-cell actions, which is why this is no
/// longer "which mouse button": a drop and a consume both name one cell and pair with
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryClickKind {
    /// Pick or move the complete authoritative stack.
    Full,
    /// Pick or move half the authoritative stack, rounded up.
    Split,
    /// Ask the server to put this cell's whole stack on the ground.
    Drop,
    /// Ask the server to consume one item from this cell.
    Consume,
}

/// A click on one slot in the inventory screen.
///
/// The UI reports only the slot and what the press was asking for. This module owns the
/// source/destination pairing and is the only place that turns it into wire intent.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryClick {
    pub slot: u8,
    pub kind: InventoryClickKind,
}

/// One consume request left this client from the hotbar. Cosmetic feedback reads it;
/// nothing else does.
///
/// **The same shape, and the same reason, as `super::combat`'s `SwingSent`.** It is written
/// on the frame a `ConsumeRequest` was *queued*, so the hand plays for a frame that left
/// rather than for a press that was made: a dropped frame is not a request, and animating one
/// would tell the player they ate when nothing was asked of the server. It is emphatically
/// not feedback for the *outcome* — whether the server spends the stack and restores any
/// hunger is its own answer, and it arrives as the next complete `InventoryState` and
/// `PlayerVitals` like every other one.
///
/// It carries nothing. There is one eating arc for every food, so there is no id for
/// `super::hands` to route on, and a message with no payload cannot grow into a second
/// opinion about what was eaten.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ConsumeSent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Picked {
    slot: u8,
    split: bool,
}

/// The source cell picked for the next move request.
///
/// This never owns a stack or a count. Those remain in [`Inventory`], untouched, until
/// another complete `InventoryState` arrives. The resource exists only so a second click
/// can name the destination of one request and the UI can outline the chosen source.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PickedStack(Option<Picked>);

impl PickedStack {
    /// The source slot waiting for a destination.
    pub fn slot(&self) -> Option<u8> {
        self.0.map(|picked| picked.slot)
    }
}

/// Orders targeting and the UI after the newest inventory and this frame's selection.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApplyInventory;

pub(super) struct InventoryPlugin;

impl Plugin for InventoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Inventory>()
            .init_resource::<SelectedSlot>()
            .init_resource::<PickedStack>()
            // PlayerPlugin owns all three in the game, while InventoryPlugin's headless
            // contract stays complete when the module is tested on its own.
            .init_resource::<InputMode>()
            .init_resource::<InputCadence>()
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .add_message::<InventoryClick>()
            .add_message::<ConsumeSent>()
            // NetPlugin initialises the same inbox. Whichever plugin is built first
            // creates it and the other finds it.
            .init_resource::<InventoryInbox>()
            .add_systems(
                Update,
                (
                    ingest_inventory,
                    select_hotbar,
                    consume_selected,
                    request_inventory_action,
                )
                    .chain()
                    .in_set(ApplyInventory)
                    .after(crate::net::DrainNetwork)
                    .after(ApplyInputMode)
                    // And after the vitals this frame's snapshot carried, so the death
                    // gate below is never read a frame stale. Ordering against a set with
                    // no systems in it is a no-op, which is what keeps this plugin usable
                    // by its own tests with no `PlayerPlugin` built at all.
                    .after(ApplySnapshots),
            );
    }
}

/// Replaces the local mirror with the newest complete state in this frame.
fn ingest_inventory(
    mut inbox: ResMut<InventoryInbox>,
    mut inventory: ResMut<Inventory>,
    mut picked: ResMut<PickedStack>,
) {
    let Some(state) = inbox.take().into_iter().last() else {
        return;
    };

    set_if_changed(
        &mut inventory,
        Inventory {
            stacks: state.stacks,
            silver: state.silver,
        },
    );
    // Any source was chosen against an older complete snapshot. Keeping it would make a
    // later click ask about a stack the screen no longer shows.
    set_if_changed(&mut picked, PickedStack::default());
}

/// Selects a hotbar slot by index with keys 1 through 9 or the mouse wheel.
fn select_hotbar(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    session: Option<Res<Session>>,
    gate: InputGate<'_>,
    mut selected: ResMut<SelectedSlot>,
) {
    // Death is read here exactly as a UI mode is: the selection survives it untouched, so
    // a respawned player comes back holding what they were holding. Nothing is chosen for
    // them and nothing is cleared.
    if !gate.may_act() {
        return;
    }
    let Some(session) = session else {
        return;
    };
    let selectable = usize::from(session.0.hotbar_slots).min(HOTBAR_KEYS.len());

    if let Some(keys) = keys {
        for (index, key) in HOTBAR_KEYS.into_iter().take(selectable).enumerate() {
            if keys.just_pressed(key) {
                set_if_changed(&mut selected, SelectedSlot(index as u8));
                return;
            }
        }
    }

    let Some(direction) = scroll
        .filter(|scroll| scroll.delta.y != 0.0)
        .map(|scroll| scroll.delta.y.signum())
    else {
        return;
    };
    let slots = session.0.hotbar_slots;
    let current = selected.0.min(slots - 1);
    let next = if direction < 0.0 {
        (current + 1) % slots
    } else {
        (current + slots - 1) % slots
    };
    set_if_changed(&mut selected, SelectedSlot(next));
}

/// Everything the hotbar's own consume press reads, in one bundle.
///
/// A `SystemParam` for the reason `player/loot.rs`'s `LootIntent` is one: the question is
/// *may this frame's consume press leave, and for which slot* — one question with one place
/// to be asked — and spelling it as eight parameters would put the system past clippy's
/// argument bound for no gain in clarity.
#[derive(bevy::ecs::system::SystemParam)]
struct ConsumeIntent<'w> {
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    settings: Option<Res<'w, Settings>>,
    session: Option<Res<'w, Session>>,
    inventory: Res<'w, Inventory>,
    selected: Res<'w, SelectedSlot>,
    gate: InputGate<'w>,
    cadence: Res<'w, InputCadence>,
    outbound: Option<ResMut<'w, Outbound>>,
}

/// Asks to eat what the hotbar has selected, once per press, while the world owns the input.
///
/// **The pack's consume press is unchanged and this is not a second copy of it.** Both
/// spellings — a cell press in [`InputMode::Inventory`], and this key in play — end at
/// [`consume_request`], which is still the only place a [`ConsumeRequest`] is built and the
/// only place [`FOODS`] is read. What differs is which slot is named: the pack names the cell
/// under the pointer, and this names [`SelectedSlot`], which the hotbar has already chosen
/// out of the leading slots of the very same authoritative inventory.
///
/// **One press is one request**, which is the rule `player/loot.rs` states for the other
/// request this client originates from a bound key: `just_pressed` is edge-triggered, and a
/// held key or a `KeyboardInput { repeat: true }` re-arms nothing.
///
/// **[`InputGate::may_act`] is the whole of the mode and death question**, so a screen that
/// owns the pointer — the pack, chat, the pause menu, the map, a stall, a corpse — closes
/// this press with no list of modes to keep in step with the enum. The frame a mode changes
/// on is closed too, which is what keeps the key that *left* the pack from also eating on its
/// way out. `ui/inventory.rs` reads the same key only while the pack is open, so the two
/// readers are mutually exclusive by mode and one press can never become two frames.
///
/// **Nothing here decides that anything was eaten.** The server re-reads its own
/// `restoresHunger` column and answers with a complete `InventoryState` and `PlayerVitals`,
/// or with silence. The stack count, the hunger bar and the pack all move then and only then;
/// what leaves this function is a request and, when that request was queued, one cosmetic
/// [`ConsumeSent`] for the hand to play.
fn consume_selected(intent: ConsumeIntent<'_>, mut eaten: MessageWriter<ConsumeSent>) {
    let ConsumeIntent {
        keys,
        settings,
        session,
        inventory,
        selected,
        gate,
        cadence,
        mut outbound,
    } = intent;
    if !gate.may_act() {
        return;
    }
    // An absent `Settings` falls back to the default bindings, the pattern `player/loot.rs`
    // already follows, so a headless app without the resource still routes the default key.
    let bindings = settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());
    if !keys.is_some_and(|keys| keys.just_pressed(bindings.key(Control::Consume))) {
        return;
    }
    let (Some(session), Some(outbound)) = (session, outbound.as_deref_mut()) else {
        return;
    };
    let Some(request) = consume_request(
        &inventory,
        selected.0,
        cadence.client_tick,
        session.0.inventory_slots,
    ) else {
        return;
    };
    match outbound.send(encode_consume_request(&request)) {
        // The arc plays for a frame that left, and for nothing else. See [`ConsumeSent`].
        Sent::Queued => {
            eaten.write(ConsumeSent);
        }
        Sent::Dropped => warn!(
            "the outbound queue was full; consuming from slot {} never reached the server",
            request.slot
        ),
        // The session is ending. There is nowhere to send and nothing to animate.
        Sent::Closed => {}
    }
}

/// Pairs a picked source with a destination and sends one intent for it.
///
/// A first click stores only a slot index. A second click on another in-range slot sends
/// one request and — for a move — clears that local cursor. At no point is [`Inventory`]
/// mutated: the server's next complete state is the only thing that can move a displayed
/// count or a displayed durability.
///
/// **A mend is the one pair that is not a move**, and it is also the one that keeps its
/// cursor: the stone stays picked so a second worn item is one more click rather than
/// another trip to the stone's slot. See [`repair_request`] for what makes a pair a mend.
///
/// **A drop and a consume are not pairs at all**, which is why both are answered before
/// anything below them reads the cursor. Each names one cell, sends at most one intent, and
/// leaves the cursor exactly as it found it: a picked slot is a source waiting for a
/// destination, and neither independent gesture is that destination. Clearing it would
/// silently cancel a move the player was half-way through.
fn request_inventory_action(
    mut clicks: MessageReader<InventoryClick>,
    session: Option<Res<Session>>,
    inventory: Res<Inventory>,
    gate: InputGate<'_>,
    cadence: Res<InputCadence>,
    outbound: Option<ResMut<Outbound>>,
    mut picked: ResMut<PickedStack>,
) {
    // The inventory screen is closed while the server says this player is dead, and the
    // toggle that would reopen it is refused in `ui/mod.rs`. This is the wire half of that
    // rule rather than a second copy of it: a click that arrived on the frame they died
    // must not be replayed into a move when they come back, and no request may leave here
    // in the meantime. The server would refuse one anyway; not sending it is the honest
    // shape of a screen that is not open.
    if gate.dead() {
        set_if_changed(&mut picked, PickedStack::default());
        clicks.read().for_each(drop);
        return;
    }

    if gate.mode() != InputMode::Inventory {
        if gate.mode.is_changed() {
            set_if_changed(&mut picked, PickedStack::default());
        }
        // Drain clicks that arrived while another mode owned the pointer. Replaying them
        // when the inventory next opens would turn an old menu click into a move.
        clicks.read().for_each(drop);
        return;
    }

    let Some(session) = session else {
        clicks.read().for_each(drop);
        set_if_changed(&mut picked, PickedStack::default());
        return;
    };
    let slots = session.0.inventory_slots;
    let mut outbound = outbound;

    for click in clicks.read().copied() {
        if click.slot >= slots {
            continue;
        }

        if click.kind == InventoryClickKind::Consume {
            // Like a drop, eating names one cell and is unrelated to the source cursor.
            // It runs ahead of the pair below and leaves that cursor untouched, so a
            // consume press cannot silently cancel a move or repair in progress.
            if let Some(request) =
                consume_request(&inventory, click.slot, cadence.client_tick, slots)
                && let Some(outbound) = outbound.as_deref_mut()
            {
                match outbound.send(encode_consume_request(&request)) {
                    Sent::Queued => {}
                    Sent::Dropped => warn!(
                        "the outbound queue was full; consuming from slot {} never reached the server",
                        request.slot
                    ),
                    Sent::Closed => {}
                }
            }
            continue;
        }

        if click.kind == InventoryClickKind::Drop {
            // Ahead of the cursor, deliberately: a drop names one cell and pairs with
            // nothing, so neither branch below applies to it. The cursor is left alone —
            // see this function's own note.
            if let Some(request) = drop_request(&inventory, click.slot, cadence.client_tick, slots)
                && let Some(outbound) = outbound.as_deref_mut()
            {
                match outbound.send(encode_drop_item_request(&request)) {
                    Sent::Queued => {}
                    Sent::Dropped => warn!(
                        "the outbound queue was full; a drop of slot {} never reached the server",
                        request.slot
                    ),
                    Sent::Closed => {}
                }
            }
            continue;
        }

        let Some(source) = picked.0 else {
            if !inventory
                .slot(click.slot)
                .is_some_and(|stack| stack.count > 0)
            {
                continue;
            }
            // Reading the slot above proves this is a real stack, but the cursor
            // deliberately stores none of its count. A later authoritative state cannot
            // be shadowed locally.
            set_if_changed(
                &mut picked,
                PickedStack(Some(Picked {
                    slot: click.slot,
                    split: click.kind == InventoryClickKind::Split,
                })),
            );
            continue;
        };

        if source.slot == click.slot {
            set_if_changed(&mut picked, PickedStack::default());
            continue;
        }

        // The whole of this client's judgement about a click. A kit onto something that
        // wears out is a mend; everything else falls through to the move it has always
        // been, including a kit onto an empty slot or onto a stack of stone.
        if let Some(request) = repair_request(
            &inventory,
            source.slot,
            click.slot,
            cadence.client_tick,
            slots,
        ) {
            // Deliberately no `set_if_changed` on the cursor: the stone stays picked, and
            // stays outlined, so several blades are several clicks. Nothing else here
            // moves either — the count this spends and the durability it restores are the
            // server's, and both appear when the next complete state does.
            if let Some(outbound) = outbound.as_deref_mut() {
                match outbound.send(encode_repair_request(&request)) {
                    Sent::Queued => {}
                    Sent::Dropped => warn!(
                        "the outbound queue was full; a repair of slot {} with slot {} never reached the server",
                        request.target_slot, request.kit_slot
                    ),
                    Sent::Closed => {}
                }
            }
            continue;
        }

        let off_hand = slots
            .checked_sub(session.0.equipment_slots)
            .and_then(|first| (session.0.equipment_slots >= 4).then_some(first + 3));
        if off_hand == Some(click.slot)
            && inventory
                .slot(source.slot)
                .is_some_and(|stack| !equipment_item_fits(stack.item_id, 3))
        {
            set_if_changed(&mut picked, PickedStack::default());
            continue;
        }

        let count = inventory
            .slot(source.slot)
            .filter(|stack| stack.count > 0)
            .map(|stack| {
                if source.split || click.kind == InventoryClickKind::Split {
                    stack.count.div_ceil(2)
                } else {
                    stack.count
                }
            })
            .unwrap_or(0);
        let request = move_request(source.slot, click.slot, count, slots);
        set_if_changed(&mut picked, PickedStack::default());

        let (Some(request), Some(outbound)) = (request, outbound.as_deref_mut()) else {
            continue;
        };
        match outbound.send(encode_inventory_move_request(&request)) {
            Sent::Queued => {}
            Sent::Dropped => warn!(
                "the outbound queue was full; an inventory move from {} to {} never reached the server",
                request.from, request.to
            ),
            Sent::Closed => {}
        }
    }
}

/// Constructs only requests the contract permits the client to name.
fn move_request(from: u8, to: u8, count: u16, slots: u8) -> Option<InventoryMoveRequest> {
    (from < slots && to < slots && from != to && count > 0).then_some(InventoryMoveRequest {
        from,
        to,
        count,
    })
}

/// Whether one cell is worth asking to put on the ground, and the request if it is.
///
/// **Two questions and no third**, which is the whole of this client's judgement here: is
/// the index one the contract permits, and does the last complete state the server sent show
/// something in it. Both are re-checked for the reason [`move_request`] re-checks its own —
/// this is the only place a [`DropItemRequest`] is built, so it is the only place they have
/// to hold. `slots` is `ServerWelcome.inventory_slots`.
///
/// **What it deliberately does not ask is whether the drop would be allowed.** The server
/// refuses a slot that wears out, because a drop carries an item id and a count and nothing
/// else. Mirroring that here would be this client deciding a gameplay outcome from a pack one
/// message old, and it is the failure direction [`super::combat::LEFT_BUTTON_USES`] records: a courtesy
/// that guesses wrong refuses what the server would have granted. The honest shape of a
/// refused drop is a stack that stays where it is.
///
/// The empty-cell check is not that — it is the same courtesy that stops an empty slot being
/// *picked* two branches down.
fn drop_request(
    inventory: &Inventory,
    slot: u8,
    client_tick: u32,
    slots: u8,
) -> Option<DropItemRequest> {
    let stack = inventory.slot(slot)?;

    (slot < slots && stack.count > 0).then_some(DropItemRequest { slot, client_tick })
}

/// Every item id this client routes a consume press to consumption for.
///
/// Which press that is belongs to `ui/inventory.rs`: the rebindable `Control::Consume`
/// key and the fixed middle-click both arrive here as one `InventoryClickKind::Consume`,
/// so this list — and every check below it — is read once per intent rather than once per
/// spelling.
///
/// A table rather than an `item_id == ITEM_RAW_MEAT` comparison, following [`KITS`]: a
/// second food is an entry, not another branch. This remains routing, never eligibility
/// authority. The server re-reads its `restoresHunger` registry column and may refuse any
/// request silently; an extra entry here can grant nothing, while a missing entry would
/// make server-supported food unreachable from this UI.
const FOODS: &[u16] = &[ITEM_RAW_MEAT, ITEM_COOKED_MEAT];

fn item_is_food(item_id: u16) -> bool {
    FOODS.contains(&item_id)
}

/// Whether one cell is worth asking the server to consume from.
///
/// Bounds, a non-empty stack and the routing table are the whole courtesy. Hunger, life
/// state and restoration capacity are deliberately absent: they are gameplay decisions
/// made against server-owned state. `slots` is `ServerWelcome.inventory_slots`.
fn consume_request(
    inventory: &Inventory,
    slot: u8,
    client_tick: u32,
    slots: u8,
) -> Option<ConsumeRequest> {
    let stack = inventory.slot(slot)?;

    (slot < slots && stack.count > 0 && item_is_food(stack.item_id)).then_some(ConsumeRequest {
        slot: u16::from(slot),
        client_tick,
    })
}

/// Every item id this client routes a click onto a worn item to a mend for.
///
/// **A table rather than a comparison, and that is the whole of the change**, exactly as
/// [`super::combat::item_is_a_blade`] is a table rather than the `item_id ==
/// ITEM_RUSTY_SWORD` it replaced. `kit.item_id == ITEM_SHARPENING_STONE` could not express
/// a second kit, so the leather patch #92 landed on the server was craftable, drawable,
/// spendable by the simulation — and unusable from this screen, because the one branch
/// that turns a pair of clicks into a `RepairRequest` named the other kit by hand. A third
/// kit is one more line here, as it is one more `repairRestore` there, and neither is an
/// edit to the predicate.
///
/// It stays this client's own opinion and it still decides nothing: the server re-reads
/// its own registry for every mend, where being a kit is a non-zero `repairRestore` rather
/// than a number in this list. A wrong entry costs a request that is refused; the failure
/// that actually costs something is the other direction — an item the server would have
/// honoured and this list omitted, which is what the leather patch silently was.
const KITS: &[u16] = &[ITEM_SHARPENING_STONE, ITEM_LEATHER_PATCH];

/// Display-side routing from an equipment-slot offset to the item ids it accepts:
/// head `0`, chest `1`, legs `2`, off-hand `3`.
///
/// This is deliberately only a routing table for the cells that draw or grey a drop
/// target. The server re-reads `itemRegistry.wornAt` before every move, so an entry here
/// can grant nothing and an omitted entry can only make the client less helpful.
pub(crate) const EQUIPMENT_ROUTES: [&[u16]; 4] = [
    &[ITEM_LEATHER_CAP, ITEM_IRON_HELM],
    &[ITEM_LEATHER_JERKIN, ITEM_IRON_CUIRASS],
    &[ITEM_LEATHER_LEGGINGS, ITEM_IRON_GREAVES],
    &[ITEM_WOODEN_SHIELD],
];

pub(crate) fn equipment_item_fits(item_id: u16, offset: u8) -> bool {
    EQUIPMENT_ROUTES
        .get(usize::from(offset))
        .is_some_and(|allowed| allowed.contains(&item_id))
}

/// Whether this client routes a click with one item id onto a worn item to a mend.
///
/// Split from [`repair_request`] for the reason [`super::combat::item_is_a_blade`] is
/// split from `blade_in_hand`: this asks about an *item*, and that asks about the two
/// *slots* the server last sent, which also have to hold a stack and something that wears
/// out. The split is what lets the sweep in the tests below hold the list itself against
/// the display registry, with no pack to build and no durability to choose.
fn item_is_a_repair_kit(item_id: u16) -> bool {
    KITS.contains(&item_id)
}

/// Whether one picked slot and one clicked slot are a mend, and the request if they are.
///
/// **Read from the two slots the server last sent and from nothing else.** Durability is
/// already beside every stack, so `max_durability > 0` answers *does this wear out* without
/// a registry, an item list or a second copy of the server's table: a resource and an empty
/// slot both carry a maximum of zero, and a blade worn through carries a current value of
/// zero under a non-zero maximum — which is exactly the item this gesture exists for.
///
/// What it deliberately does **not** ask is whether the mend would achieve anything. A
/// target already at full durability is refused server-side, and asking it here would be
/// this client deciding a gameplay outcome from a state that is one message old. legacy PR 110
/// makes every refusal silence, so the honest shape of a mend that did not happen is a
/// durability bar that did not move.
///
/// Which ids are kits is presentation and routing, exactly as `super::combat`'s `BLADES`
/// is: the server reads its own registry, where being a kit is a non-zero `repairRestore`
/// rather than membership of a list, so [`KITS`] cannot make another item mend and cannot
/// make one of its own legal.
///
/// The bounds and the two-different-slots rule are re-checked here for the reason
/// [`move_request`] re-checks its own: this is the only place a `RepairRequest` is built,
/// so it is the only place that has to hold. `slots` is `ServerWelcome.inventory_slots`.
fn repair_request(
    inventory: &Inventory,
    kit_slot: u8,
    target_slot: u8,
    client_tick: u32,
    slots: u8,
) -> Option<RepairRequest> {
    let kit = inventory.slot(kit_slot)?;
    let target = inventory.slot(target_slot)?;

    (kit_slot < slots
        && target_slot < slots
        && kit_slot != target_slot
        && item_is_a_repair_kit(kit.item_id)
        && kit.count > 0
        && target.max_durability > 0)
        .then_some(RepairRequest {
            kit_slot,
            target_slot,
            client_tick,
        })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
    use bevy::input::{ButtonState, InputPlugin};

    use super::*;
    use crate::net::{InventoryState, SessionParams};
    use crate::player::crafting::ITEM_IRON_SWORD;
    use crate::player::items::{ITEM_STONE, ITEM_VARGR_PELT, item_label};
    use crate::settings::Bindings;
    use crate::wire::voxelheim::net as fb;

    fn stack(item_id: u16, count: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count,
            ..Default::default()
        }
    }

    /// One item that wears out, as a server state carries it: one to a slot, with the
    /// wear it has left under the maximum it can hold.
    fn worn(item_id: u16, durability: u16, max_durability: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count: 1,
            durability,
            max_durability,
        }
    }

    /// A four-slot pack with these stacks placed by index and every other slot empty.
    fn pack(values: &[(usize, InventoryStack)]) -> Vec<InventoryStack> {
        let mut pack = vec![InventoryStack::default(); 4];
        for &(slot, held) in values {
            pack[slot] = held;
        }
        pack
    }

    fn slots(values: &[(usize, u16, u16)]) -> Vec<InventoryStack> {
        let mut slots = vec![stack(0, 0); 4];
        for &(slot, item_id, count) in values {
            slots[slot] = stack(item_id, count);
        }
        slots
    }

    fn app(with_input: bool) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(InventoryPlugin)
            .insert_resource(Session(SessionParams {
                clock: Default::default(),
                entity_id: 1,
                spawn: [0.0; 3],
                world_seed: 0,
                tick_rate: 20,
                chunk_size: 32,
                view_distance: 3,
                inventory_slots: 5,
                hotbar_slots: 4,
                equipment_slots: 1,
                player_token: crate::net::ANY_TOKEN,
            }));
        if with_input {
            app.add_plugins(InputPlugin);
        }
        app
    }

    fn deliver(app: &mut App, stacks: Vec<InventoryStack>) {
        deliver_state(app, stacks, 0);
    }

    fn deliver_state(app: &mut App, stacks: Vec<InventoryStack>, silver: u32) {
        app.world_mut()
            .resource_mut::<InventoryInbox>()
            .push(InventoryState { stacks, silver });
    }

    fn press(app: &mut App, key_code: KeyCode, character: &str) {
        app.world_mut().write_message(KeyboardInput {
            key_code,
            logical_key: Key::Character(character.into()),
            state: ButtonState::Pressed,
            text: Some(character.into()),
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
    }

    #[test]
    fn the_newest_inventory_replaces_the_previous_one_whole() {
        let mut app = app(false);
        deliver_state(&mut app, slots(&[(0, 1, 3), (1, 2, 4)]), 17);
        app.update();
        assert_eq!(
            app.world().resource::<Inventory>().stacks(),
            slots(&[(0, 1, 3), (1, 2, 4)])
        );
        assert_eq!(app.world().resource::<Inventory>().silver(), 17);

        deliver_state(&mut app, slots(&[(2, 2, 1)]), 29);
        app.update();
        let inventory = app.world().resource::<Inventory>();
        assert_eq!(inventory.stacks(), slots(&[(2, 2, 1)]));
        assert_eq!(inventory.silver(), 29);
        assert_eq!(
            inventory.count(1),
            0,
            "the old stack was merged instead of replaced"
        );
    }

    #[test]
    fn only_the_last_complete_state_in_one_frame_matters() {
        let mut app = app(false);
        deliver(&mut app, slots(&[(0, 1, 1)]));
        deliver(&mut app, slots(&[(0, 1, 2), (3, 3, 5)]));
        app.update();

        assert_eq!(
            app.world().resource::<Inventory>().stacks(),
            slots(&[(0, 1, 2), (3, 3, 5)])
        );
    }

    #[test]
    fn number_keys_change_selection_without_changing_a_count() {
        let mut app = app(true);
        deliver(&mut app, slots(&[(0, 3, 7), (1, 1, 4)]));
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(0));

        press(&mut app, KeyCode::Digit2, "2");
        app.update();

        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(1));
        assert_eq!(
            app.world().resource::<Inventory>().stacks(),
            slots(&[(0, 3, 7), (1, 1, 4)])
        );
    }

    #[test]
    fn selection_is_a_slot_and_does_not_follow_items_between_states() {
        let mut app = app(false);
        app.world_mut().resource_mut::<SelectedSlot>().0 = 2;
        deliver(&mut app, slots(&[(2, 2, 1)]));
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(2));

        deliver(&mut app, slots(&[]));
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(2));
        assert_eq!(
            app.world().resource::<Inventory>().slot(2),
            Some(stack(0, 0))
        );
    }

    #[test]
    fn the_mouse_wheel_wraps_the_hotbar_selection() {
        let mut app = app(false);
        app.update();

        app.insert_resource(AccumulatedMouseScroll {
            delta: Vec2::new(0.0, -1.0),
            ..default()
        });
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(1));

        app.insert_resource(AccumulatedMouseScroll {
            delta: Vec2::new(0.0, 1.0),
            ..default()
        });
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(0));

        app.insert_resource(AccumulatedMouseScroll {
            delta: Vec2::new(0.0, 1.0),
            ..default()
        });
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(3));
    }

    fn move_app() -> (App, Receiver<Vec<u8>>) {
        let mut app = app(false);
        let (outbound, sent) = Outbound::to_a_test(16);
        app.insert_resource(outbound);
        deliver(&mut app, slots(&[(0, 1, 5), (1, 2, 4)]));
        app.update();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();
        (app, sent)
    }

    fn inventory_click(app: &mut App, slot: u8, kind: InventoryClickKind) {
        app.world_mut().write_message(InventoryClick { slot, kind });
        app.update();
    }

    #[test]
    fn a_source_and_destination_emit_exactly_one_move_without_changing_counts() {
        let (mut app, sent) = move_app();
        let before = app.world().resource::<Inventory>().clone();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        assert!(
            sent.try_recv().is_err(),
            "picking a source has no destination"
        );
        inventory_click(&mut app, 2, InventoryClickKind::Full);

        assert_eq!(
            sent.try_recv().expect("one move was sent"),
            encode_inventory_move_request(&InventoryMoveRequest {
                from: 0,
                to: 2,
                count: 5,
            })
        );
        assert!(
            sent.try_recv().is_err(),
            "one action sent more than one frame"
        );
        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "a request changed the last server-sent state"
        );
    }

    #[test]
    fn a_right_click_requests_half_the_authoritative_stack() {
        let (mut app, sent) = move_app();

        inventory_click(&mut app, 0, InventoryClickKind::Split);
        inventory_click(&mut app, 3, InventoryClickKind::Full);

        assert_eq!(
            sent.try_recv().expect("the split move was sent"),
            encode_inventory_move_request(&InventoryMoveRequest {
                from: 0,
                to: 3,
                count: 3,
            })
        );
    }

    #[test]
    fn clicking_the_same_slot_twice_cancels_without_a_request() {
        let (mut app, sent) = move_app();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 0, InventoryClickKind::Full);

        assert!(sent.try_recv().is_err());
        assert_eq!(app.world().resource::<PickedStack>().slot(), None);
    }

    #[test]
    fn an_out_of_range_click_never_becomes_a_request() {
        let (mut app, sent) = move_app();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 5, InventoryClickKind::Full);

        assert!(sent.try_recv().is_err());
        assert_eq!(
            app.world().resource::<PickedStack>().slot(),
            Some(0),
            "an invalid destination consumed the valid source"
        );
        assert!(move_request(0, 5, 1, 5).is_none());
    }

    #[test]
    fn a_refused_move_needs_no_rollback() {
        let (mut app, _sent) = move_app();
        let before = app.world().resource::<Inventory>().clone();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 2, InventoryClickKind::Full);
        for _ in 0..4 {
            app.update();
        }

        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "silence from the server required a local rollback"
        );
    }

    #[test]
    fn a_non_off_hand_item_sends_nothing_to_the_off_hand_slot() {
        let mut app = app(false);
        let mut params = app.world().resource::<Session>().0;
        params.inventory_slots = 8;
        params.hotbar_slots = 4;
        params.equipment_slots = 4;
        app.insert_resource(Session(params));
        let (outbound, sent) = Outbound::to_a_test(16);
        app.insert_resource(outbound);
        deliver(
            &mut app,
            vec![
                stack(ITEM_STONE, 1),
                InventoryStack::default(),
                InventoryStack::default(),
                InventoryStack::default(),
                InventoryStack::default(),
                InventoryStack::default(),
                InventoryStack::default(),
                InventoryStack::default(),
            ],
        );
        app.update();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 7, InventoryClickKind::Full);

        assert!(sent.try_recv().is_err());
        assert_eq!(app.world().resource::<PickedStack>().slot(), None);
    }

    /// Replaces the vitals exactly as an accepted snapshot does.
    fn say_dead(app: &mut App, dead: bool) {
        app.insert_resource(SelfVitals::from_server(crate::net::PlayerVitals {
            health: if dead { 0 } else { 100 },
            max_health: 100,
            hunger: 100,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state: if dead {
                crate::net::LifeState::Dead
            } else {
                crate::net::LifeState::Alive
            },
            respawn_ticks: if dead { 60 } else { 0 },
            invulnerable: false,
            blocking: false,
        }));
    }

    #[test]
    fn a_dead_player_originates_no_move_and_keeps_no_picked_source() {
        // The screen these clicks come from is closed while the server says the player is
        // dead, so a click that arrived on the frame they died must not be replayed into a
        // move when they come back. Nothing local is spent either way: the counts are the
        // server's and remain untouched.
        let (mut app, sent) = move_app();
        let before = app.world().resource::<Inventory>().clone();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        assert_eq!(app.world().resource::<PickedStack>().slot(), Some(0));

        say_dead(&mut app, true);
        inventory_click(&mut app, 1, InventoryClickKind::Full);

        assert!(sent.try_recv().is_err(), "a dead player asked for a move");
        assert_eq!(app.world().resource::<PickedStack>().slot(), None);

        // Coming back is not a replay: the click that arrived while dead is gone.
        say_dead(&mut app, false);
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        for _ in 0..3 {
            app.update();
        }
        assert!(sent.try_recv().is_err());
        assert_eq!(*app.world().resource::<Inventory>(), before);
    }

    // -----------------------------------------------------------------------
    // Mending by hand — the one click that is not a move
    // -----------------------------------------------------------------------

    /// What one frame asked the server for, read back through the generated accessors.
    ///
    /// Every shape in one enum on purpose: the whole feature is which of the four leaves,
    /// and a helper that could only see repairs would report a move as nothing at all.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Asked {
        Move { from: u8, to: u8, count: u16 },
        Repair { kit: u8, target: u8 },
        Drop { slot: u8 },
        Consume { slot: u16 },
    }

    /// Everything the client sent, in the order it left.
    fn asked(sent: &Receiver<Vec<u8>>) -> Vec<Asked> {
        let mut found = Vec::new();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            if let Some(request) = envelope.payload_as_repair_request() {
                found.push(Asked::Repair {
                    kit: request.kit_slot(),
                    target: request.target_slot(),
                });
            } else if let Some(request) = envelope.payload_as_inventory_move_request() {
                found.push(Asked::Move {
                    from: request.from(),
                    to: request.to(),
                    count: request.count(),
                });
            } else if let Some(request) = envelope.payload_as_drop_item_request() {
                found.push(Asked::Drop {
                    slot: request.slot(),
                });
            } else if let Some(request) = envelope.payload_as_consume_request() {
                found.push(Asked::Consume {
                    slot: request.slot(),
                });
            } else {
                panic!(
                    "the inventory sent a {:?}, which is not one of its four intents",
                    envelope.payload_type()
                );
            }
        }
        found
    }

    /// An open inventory screen holding exactly these stacks, and a socket to read.
    fn mend_app(stacks: Vec<InventoryStack>) -> (App, Receiver<Vec<u8>>) {
        let mut app = app(false);
        let (outbound, sent) = Outbound::to_a_test(16);
        app.insert_resource(outbound);
        deliver(&mut app, stacks);
        app.update();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();
        (app, sent)
    }

    /// The routing matrix, read straight off the two slots and nothing else.
    ///
    /// A table rather than six app tests because this is one predicate with one job, and
    /// the boundary between mend and move is the whole of what it has to get right.
    #[test]
    fn a_kit_onto_something_that_wears_out_is_the_only_pair_that_mends() {
        let stone = stack(ITEM_SHARPENING_STONE, 3);
        for (name, kit, target, want) in [
            (
                "a stone onto a worn blade",
                stone,
                worn(ITEM_IRON_SWORD, 40, 100),
                true,
            ),
            (
                "a stone onto a blade worn through — the item this gesture exists for",
                stone,
                worn(ITEM_IRON_SWORD, 0, 100),
                true,
            ),
            (
                "a stone onto a full blade — the refusal is the server's to make",
                stone,
                worn(ITEM_IRON_SWORD, 100, 100),
                true,
            ),
            (
                "a stone onto an item id this build has never heard of, that wears out",
                stone,
                worn(4242, 5, 60),
                true,
            ),
            (
                "a leather patch onto a worn blade — the pair that was a move until #113",
                stack(ITEM_LEATHER_PATCH, 2),
                worn(ITEM_IRON_SWORD, 40, 100),
                true,
            ),
            (
                "a vargr pelt onto a worn blade — two of them make a kit, and one is not",
                stack(ITEM_VARGR_PELT, 2),
                worn(ITEM_IRON_SWORD, 40, 100),
                false,
            ),
            (
                "a stone onto an empty slot",
                stone,
                InventoryStack::default(),
                false,
            ),
            (
                "a stone onto a stack of stone",
                stone,
                stack(ITEM_STONE, 12),
                false,
            ),
            (
                "a stone onto more stones",
                stone,
                stack(ITEM_SHARPENING_STONE, 2),
                false,
            ),
            (
                "a blade onto a worn blade — a picked non-kit never mends",
                worn(ITEM_IRON_SWORD, 90, 100),
                worn(ITEM_IRON_SWORD, 40, 100),
                false,
            ),
            (
                "a stack of stone onto a worn blade",
                stack(ITEM_STONE, 9),
                worn(ITEM_IRON_SWORD, 40, 100),
                false,
            ),
            (
                "a kit slot the server emptied under the cursor",
                stack(ITEM_SHARPENING_STONE, 0),
                worn(ITEM_IRON_SWORD, 40, 100),
                false,
            ),
        ] {
            let inventory = Inventory::from_stacks(vec![kit, target]);
            assert_eq!(
                repair_request(&inventory, 0, 1, 7, 2).is_some(),
                want,
                "{name}"
            );
        }

        // Nothing mends itself, and no index may leave the pack the server announced.
        let inventory = Inventory::from_stacks(vec![stone, worn(ITEM_IRON_SWORD, 40, 100)]);
        assert!(repair_request(&inventory, 0, 0, 7, 2).is_none());
        assert!(repair_request(&inventory, 0, 1, 7, 1).is_none());
        assert!(repair_request(&inventory, 0, 9, 7, 2).is_none());
    }

    /// Every kit this list names mends, is named by the registry, and appears once.
    ///
    /// **Swept over [`KITS`] rather than over the ids written into the tests around it**,
    /// which is what makes a third kit a decision made in that list rather than a branch
    /// nobody adds. The three questions are the three ways an *entry* can be wrong: an id
    /// the predicate does not honour, an id the panel would draw as "unknown item", and an
    /// id typed twice.
    ///
    /// **What it cannot see is an entry that is missing**, and that limit is worth stating
    /// rather than leaving for somebody to discover. `player::crafting`'s sweep can read
    /// the contract because `RecipeID` is on the wire; nothing enumerates kits on this
    /// side, because what makes an item a kit is a non-zero `repairRestore` in the
    /// server's registry and that registry is deliberately never sent. So the leather
    /// patch is pinned by name in the test below — the omission this list existed to end
    /// is the one shape a sweep over the list is blind to.
    #[test]
    fn every_kit_mends_is_named_and_appears_once() {
        assert!(!KITS.is_empty(), "no id routes a mend at all");

        for &kit in KITS {
            assert!(
                item_is_a_repair_kit(kit),
                "kit {kit} is not routed by its own list"
            );
            assert_ne!(
                item_label(kit),
                "unknown item",
                "kit {kit} routes a mend and the panel cannot name it"
            );

            let inventory =
                Inventory::from_stacks(vec![stack(kit, 1), worn(ITEM_IRON_SWORD, 40, 100)]);
            assert!(
                repair_request(&inventory, 0, 1, 7, 2).is_some(),
                "kit {kit} onto a worn blade is not a mend"
            );
        }

        // The other direction, and the proof the sweep is not passing vacuously: a list
        // that honoured everything would satisfy the loop above perfectly.
        for not_a_kit in [ITEM_STONE, ITEM_VARGR_PELT, ITEM_IRON_SWORD, u16::MAX] {
            assert!(
                !item_is_a_repair_kit(not_a_kit),
                "item {not_a_kit} routes a mend"
            );
        }

        let mut once = KITS.to_vec();
        once.sort_unstable();
        once.dedup();
        assert_eq!(
            once.len(),
            KITS.len(),
            "an id appears twice in the kit list"
        );
    }

    #[test]
    fn every_equipment_route_names_one_registry_item_once() {
        assert_eq!(EQUIPMENT_ROUTES.len(), 4);
        assert_eq!(EQUIPMENT_ROUTES[3], &[ITEM_WOODEN_SHIELD]);
        for (slot, items) in EQUIPMENT_ROUTES.iter().enumerate() {
            for &item_id in *items {
                assert_ne!(
                    item_label(item_id),
                    "unknown item",
                    "equipment item {item_id}"
                );
                assert!(equipment_item_fits(item_id, slot as u8));
                assert_eq!(
                    EQUIPMENT_ROUTES
                        .iter()
                        .flat_map(|items| items.iter().copied())
                        .filter(|&other| other == item_id)
                        .count(),
                    1,
                    "equipment item {item_id} appears more than once"
                );
            }
        }
    }

    /// The mend the leather patch could not complete, and the move it used to be instead.
    ///
    /// The failure this pins is not that the gesture was refused — it is that it silently
    /// meant something else. A patch onto a worn blade fell through to the move every
    /// other pair is, so the kit landed in the blade's slot and nothing was mended.
    #[test]
    fn a_picked_leather_patch_mends_the_clicked_blade_too() {
        let (mut app, sent) = mend_app(pack(&[
            (0, stack(ITEM_LEATHER_PATCH, 2)),
            (1, worn(ITEM_IRON_SWORD, 40, 100)),
        ]));
        let before = app.world().resource::<Inventory>().clone();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 1, InventoryClickKind::Full);

        assert_eq!(asked(&sent), vec![Asked::Repair { kit: 0, target: 1 }]);
        assert_eq!(
            app.world().resource::<PickedStack>().slot(),
            Some(0),
            "the patch was put down after one mend"
        );
        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "a repair spent a patch or restored a durability locally"
        );
    }

    #[test]
    fn a_picked_stone_mends_the_clicked_blade_and_stays_picked() {
        let (mut app, sent) = mend_app(pack(&[
            (0, stack(ITEM_SHARPENING_STONE, 3)),
            (1, worn(ITEM_IRON_SWORD, 40, 100)),
            (2, worn(ITEM_IRON_SWORD, 12, 100)),
        ]));
        let before = app.world().resource::<Inventory>().clone();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        assert!(asked(&sent).is_empty(), "picking a kit asked for something");

        inventory_click(&mut app, 1, InventoryClickKind::Full);
        assert_eq!(asked(&sent), vec![Asked::Repair { kit: 0, target: 1 }]);
        assert_eq!(
            app.world().resource::<PickedStack>().slot(),
            Some(0),
            "the stone was put down after one mend"
        );

        // Which is what makes the second blade one more click rather than another trip
        // to the stone's slot.
        inventory_click(&mut app, 2, InventoryClickKind::Full);
        assert_eq!(asked(&sent), vec![Asked::Repair { kit: 0, target: 2 }]);

        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "a repair spent a stone or restored a durability locally"
        );
    }

    /// Every other pair keeps today's semantics exactly, including the split click.
    #[test]
    fn every_other_pair_is_still_the_move_it_was() {
        let (mut app, sent) = mend_app(pack(&[
            (0, stack(ITEM_SHARPENING_STONE, 4)),
            (1, worn(ITEM_IRON_SWORD, 40, 100)),
            (2, stack(ITEM_STONE, 6)),
        ]));

        // A kit onto an empty slot.
        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 3, InventoryClickKind::Full);
        assert_eq!(
            asked(&sent),
            vec![Asked::Move {
                from: 0,
                to: 3,
                count: 4
            }]
        );

        // A kit onto a resource stack, halved by the right button exactly as before.
        inventory_click(&mut app, 0, InventoryClickKind::Split);
        inventory_click(&mut app, 2, InventoryClickKind::Full);
        assert_eq!(
            asked(&sent),
            vec![Asked::Move {
                from: 0,
                to: 2,
                count: 2
            }]
        );

        // A picked non-kit onto something that wears out.
        inventory_click(&mut app, 2, InventoryClickKind::Full);
        inventory_click(&mut app, 1, InventoryClickKind::Full);
        assert_eq!(
            asked(&sent),
            vec![Asked::Move {
                from: 2,
                to: 1,
                count: 6
            }]
        );

        // And each of those put the cursor down, which a mend deliberately does not.
        assert_eq!(app.world().resource::<PickedStack>().slot(), None);
    }

    /// Whether the mend achieves anything is the server's answer, and silence is how it
    /// says no. Nothing here rolls back, because nothing here moved.
    #[test]
    fn an_unworn_blade_is_asked_about_and_a_refusal_leaves_the_view_untouched() {
        let (mut app, sent) = mend_app(pack(&[
            (0, stack(ITEM_SHARPENING_STONE, 1)),
            (1, worn(ITEM_IRON_SWORD, 100, 100)),
        ]));
        let before = app.world().resource::<Inventory>().clone();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 1, InventoryClickKind::Full);
        assert_eq!(asked(&sent), vec![Asked::Repair { kit: 0, target: 1 }]);

        for _ in 0..4 {
            app.update();
        }
        assert!(asked(&sent).is_empty(), "one click asked twice");
        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "silence from the server required a local rollback"
        );

        // And the authoritative answer is the only thing that moves the bar: a state
        // saying the stone is gone and nothing was mended is applied verbatim.
        deliver(&mut app, pack(&[(1, worn(ITEM_IRON_SWORD, 100, 100))]));
        app.update();
        assert_eq!(
            app.world().resource::<Inventory>().slot(0),
            Some(InventoryStack::default())
        );
        assert_eq!(
            app.world().resource::<Inventory>().slot(1),
            Some(worn(ITEM_IRON_SWORD, 100, 100))
        );
    }

    #[test]
    fn the_repair_carries_the_shared_client_tick() {
        let (mut app, sent) = mend_app(pack(&[
            (0, stack(ITEM_SHARPENING_STONE, 1)),
            (1, worn(ITEM_IRON_SWORD, 40, 100)),
        ]));
        app.world_mut().resource_mut::<InputCadence>().client_tick = 77;

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 1, InventoryClickKind::Full);

        let frame = sent.try_recv().expect("one repair was sent");
        let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
        let request = envelope
            .payload_as_repair_request()
            .expect("the payload is a repair request");
        assert_eq!(request.client_tick(), 77);
    }

    /// The same gate the move path is behind, and for the same reason: the screen these
    /// clicks come from is closed while the server says the player is dead.
    #[test]
    fn a_dead_player_originates_no_repair_and_keeps_no_kit_picked() {
        let (mut app, sent) = mend_app(pack(&[
            (0, stack(ITEM_SHARPENING_STONE, 1)),
            (1, worn(ITEM_IRON_SWORD, 40, 100)),
        ]));

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        say_dead(&mut app, true);
        inventory_click(&mut app, 1, InventoryClickKind::Full);

        assert!(asked(&sent).is_empty(), "a dead player mended a blade");
        assert_eq!(app.world().resource::<PickedStack>().slot(), None);
    }

    // -----------------------------------------------------------------------
    // Eating — the other one-cell gesture
    // -----------------------------------------------------------------------

    #[test]
    fn every_food_is_named_routed_and_appears_once() {
        assert!(!FOODS.is_empty(), "no id routes consumption at all");

        for &food in FOODS {
            assert!(item_is_food(food), "food {food} is not routed by its list");
            assert_ne!(
                item_label(food),
                "unknown item",
                "food {food} can be eaten but the inventory cannot name it"
            );
            let inventory = Inventory::from_stacks(vec![stack(food, 1)]);
            assert!(
                consume_request(&inventory, 0, 7, 1).is_some(),
                "food {food} does not produce a consume request"
            );
        }

        for not_food in [ITEM_STONE, ITEM_VARGR_PELT, ITEM_LEATHER_PATCH, u16::MAX] {
            assert!(
                !item_is_food(not_food),
                "item {not_food} routes consumption"
            );
        }

        let mut once = FOODS.to_vec();
        once.sort_unstable();
        once.dedup();
        assert_eq!(
            once.len(),
            FOODS.len(),
            "an id appears twice in the food list"
        );
    }

    #[test]
    fn only_a_nonempty_food_slot_inside_the_pack_is_asked_to_be_consumed() {
        for (name, inventory, slot, slots, want) in [
            (
                "raw meat",
                Inventory::from_stacks(vec![stack(ITEM_RAW_MEAT, 2)]),
                0,
                1,
                true,
            ),
            (
                "an empty food stack",
                Inventory::from_stacks(vec![stack(ITEM_RAW_MEAT, 0)]),
                0,
                1,
                false,
            ),
            (
                "a non-food",
                Inventory::from_stacks(vec![stack(ITEM_STONE, 2)]),
                0,
                1,
                false,
            ),
            (
                "a slot past the announced pack",
                Inventory::from_stacks(vec![stack(ITEM_RAW_MEAT, 2)]),
                0,
                0,
                false,
            ),
        ] {
            assert_eq!(
                consume_request(&inventory, slot, 77, slots),
                want.then_some(ConsumeRequest {
                    slot: u16::from(slot),
                    client_tick: 77,
                }),
                "{name}"
            );
        }
    }

    /// Each gesture keeps its old meaning, and consuming is independent of a picked
    /// source just like dropping is. This is the routing boundary the issue adds.
    #[test]
    fn consume_does_not_shadow_move_repair_or_drop() {
        let (mut app, sent) = mend_app(pack(&[
            (0, stack(ITEM_RAW_MEAT, 2)),
            (1, stack(ITEM_LEATHER_PATCH, 1)),
            (2, worn(ITEM_IRON_SWORD, 40, 100)),
        ]));
        let before = app.world().resource::<Inventory>().clone();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        inventory_click(&mut app, 3, InventoryClickKind::Full);
        assert_eq!(
            asked(&sent),
            vec![Asked::Move {
                from: 0,
                to: 3,
                count: 2,
            }]
        );

        inventory_click(&mut app, 1, InventoryClickKind::Full);
        inventory_click(&mut app, 2, InventoryClickKind::Full);
        assert_eq!(asked(&sent), vec![Asked::Repair { kit: 1, target: 2 }]);
        assert_eq!(app.world().resource::<PickedStack>().slot(), Some(1));

        inventory_click(&mut app, 0, InventoryClickKind::Drop);
        assert_eq!(asked(&sent), vec![Asked::Drop { slot: 0 }]);
        assert_eq!(app.world().resource::<PickedStack>().slot(), Some(1));

        inventory_click(&mut app, 0, InventoryClickKind::Consume);
        assert_eq!(asked(&sent), vec![Asked::Consume { slot: 0 }]);
        assert_eq!(
            app.world().resource::<PickedStack>().slot(),
            Some(1),
            "consuming cancelled the unfinished repair cursor"
        );
        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "one of the four requests changed authoritative state locally"
        );
    }

    #[test]
    fn a_dead_player_originates_no_consume_request() {
        let (mut app, sent) = mend_app(pack(&[(0, stack(ITEM_RAW_MEAT, 1))]));
        say_dead(&mut app, true);

        inventory_click(&mut app, 0, InventoryClickKind::Consume);

        assert!(asked(&sent).is_empty(), "a dead player asked to eat");
    }

    // -----------------------------------------------------------------------
    // Putting something back on the ground — the one click that pairs with nothing
    // -----------------------------------------------------------------------

    /// A shift-click names one cell, asks for that cell on the shared tick, and moves
    /// nothing local.
    ///
    /// The stack is still on screen after the request leaves, because the count in it is the
    /// server's: the complete `InventoryState` that follows an accepted drop is what empties
    /// the cell, and a refusal is a cell that never changes.
    #[test]
    fn a_shift_click_asks_for_the_whole_cell_to_be_put_down() {
        let (mut app, sent) = move_app();
        let before = app.world().resource::<Inventory>().clone();
        app.world_mut().resource_mut::<InputCadence>().client_tick = 77;

        inventory_click(&mut app, 1, InventoryClickKind::Drop);

        let frame = sent.try_recv().expect("one drop was sent");
        let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
        let request = envelope
            .payload_as_drop_item_request()
            .expect("the payload is a drop request");
        assert_eq!((request.slot(), request.client_tick()), (1, 77));
        assert!(
            sent.try_recv().is_err(),
            "one press sent more than one frame"
        );
        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "asking to put a stack down moved a count locally"
        );
    }

    /// A drop is not a destination, so it leaves a half-finished move alone — including
    /// when it names the picked cell itself, because the branch runs before the cursor is
    /// read at all.
    ///
    /// The cursor is a source waiting to be paired and the player can see it as an outline.
    /// Consuming it would cancel a move they were part-way through for a gesture that has
    /// nothing to do with it, and re-pointing it at the dropped cell would be worse: that
    /// cell may not exist a message later.
    #[test]
    fn a_drop_leaves_a_picked_source_exactly_where_it_was() {
        let (mut app, sent) = move_app();

        inventory_click(&mut app, 0, InventoryClickKind::Full);
        assert_eq!(app.world().resource::<PickedStack>().slot(), Some(0));

        for slot in [1, 0] {
            inventory_click(&mut app, slot, InventoryClickKind::Drop);
            assert_eq!(asked(&sent), vec![Asked::Drop { slot }]);
            assert_eq!(
                app.world().resource::<PickedStack>().slot(),
                Some(0),
                "a drop of slot {slot} consumed the source of an unfinished move"
            );
        }

        // And the move it was half-way through still completes, unchanged.
        inventory_click(&mut app, 2, InventoryClickKind::Full);
        assert_eq!(
            asked(&sent),
            vec![Asked::Move {
                from: 0,
                to: 2,
                count: 5
            }]
        );
    }

    /// An empty cell and an index past the announced pack both ask for nothing. The bound is
    /// the contract's and the emptiness is a courtesy; neither is a verdict.
    #[test]
    fn a_drop_of_nothing_or_of_a_slot_that_does_not_exist_is_never_asked_for() {
        let (mut app, sent) = move_app();

        inventory_click(&mut app, 2, InventoryClickKind::Drop); // empty in move_app's pack
        inventory_click(&mut app, 4, InventoryClickKind::Drop); // one past four slots

        assert!(asked(&sent).is_empty());
        assert!(drop_request(app.world().resource::<Inventory>(), 2, 0, 4).is_none());
        assert!(drop_request(app.world().resource::<Inventory>(), 4, 0, 4).is_none());
    }

    /// **A worn blade is asked about, and the outcome is the server's.**
    ///
    /// V11 lets the server carry its exact wear through the ground, but this side still
    /// predicts no acceptance. Filtering by durability here would be a second copy of a
    /// server rule, risking the failure `combat::LEFT_BUTTON_USES` records — a courtesy that guesses
    /// wrong and refuses what the server would have granted. So the frame leaves, and only
    /// the authoritative inventory and snapshot can show what happened.
    #[test]
    fn a_slot_that_wears_out_is_asked_about_without_a_client_side_decision() {
        let (mut app, sent) = mend_app(pack(&[(1, worn(ITEM_IRON_SWORD, 40, 100))]));
        let before = app.world().resource::<Inventory>().clone();

        inventory_click(&mut app, 1, InventoryClickKind::Drop);

        assert_eq!(
            asked(&sent),
            vec![Asked::Drop { slot: 1 }],
            "this client decided a gameplay outcome the server owns"
        );
        assert_eq!(*app.world().resource::<Inventory>(), before);
    }

    /// The same gate the move and mend paths are behind, and for the same reason: the screen
    /// this press comes from is closed while the server says the player is dead.
    #[test]
    fn a_dead_player_originates_no_drop() {
        let (mut app, sent) = move_app();

        say_dead(&mut app, true);
        inventory_click(&mut app, 0, InventoryClickKind::Drop);

        assert!(asked(&sent).is_empty(), "a dead player put something down");
    }

    #[test]
    fn a_dead_player_does_not_change_the_selected_slot() {
        // Selection survives the death untouched, so a respawned player comes back holding
        // exactly what they were holding — nothing is chosen for them and nothing cleared.
        let mut app = app(true);
        deliver(&mut app, slots(&[(0, 3, 7), (1, 1, 4)]));
        app.update();

        say_dead(&mut app, true);
        press(&mut app, KeyCode::Digit2, "2");
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(0));

        // A different key, because the first one is still held: `just_pressed` is an edge,
        // and pressing a key that is already down is not one.
        say_dead(&mut app, false);
        press(&mut app, KeyCode::Digit3, "3");
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(2));
    }
    // -----------------------------------------------------------------------
    // Eating from the hotbar — the consume key with the pack closed
    // -----------------------------------------------------------------------

    fn key_event(key: KeyCode, state: ButtonState, repeat: bool) -> KeyboardInput {
        KeyboardInput {
            key_code: key,
            logical_key: Key::Unidentified(NativeKey::Unidentified),
            state,
            text: None,
            repeat,
            window: Entity::PLACEHOLDER,
        }
    }

    /// The default [`Control::Consume`] key, read from the bindings rather than written down.
    ///
    /// The system reads a rebindable control, so a test naming `KeyCode::KeyC` by hand would
    /// keep passing after somebody moved the default and would be pinning the wrong thing.
    fn consume_key() -> KeyCode {
        Bindings::default().key(Control::Consume)
    }

    /// The inventory module with the real keyboard pipeline behind it, a pack holding these
    /// stacks, and a socket to read what left.
    fn hotbar_app(stacks: Vec<InventoryStack>) -> (App, Receiver<Vec<u8>>) {
        let mut app = app(true);
        let (outbound, sent) = Outbound::to_a_test(16);
        app.insert_resource(outbound);
        deliver(&mut app, stacks);
        // Two frames: the first ingests the pack, and the second leaves `InputMode` unchanged
        // so `may_act` is not looking at a mode that changed this frame.
        app.update();
        app.update();
        (app, sent)
    }

    /// Runs one frame after delivering the events winit would have delivered for it, and
    /// reports what left the client and what the hand was told.
    fn consume_frame(
        app: &mut App,
        sent: &Receiver<Vec<u8>>,
        events: impl IntoIterator<Item = KeyboardInput>,
    ) -> (Vec<Asked>, usize) {
        for event in events {
            app.world_mut().write_message(event);
        }
        app.update();
        let played = app
            .world_mut()
            .resource_mut::<Messages<ConsumeSent>>()
            .drain()
            .count();
        (asked(sent), played)
    }

    /// **The press in play names the selected slot and nothing else.**
    ///
    /// The hotbar is the leading slots of the same authoritative pack, so the whole of what
    /// this path adds is *which index* — and the index is the one [`SelectedSlot`] already
    /// carries for a place request. The pack it is read against is not touched: no count
    /// moves here, and the server's next complete state is the only thing that can move one.
    #[test]
    fn the_consume_key_in_play_asks_to_eat_the_selected_hotbar_slot() {
        let (mut app, sent) = hotbar_app(pack(&[
            (0, stack(ITEM_STONE, 4)),
            (1, stack(ITEM_COOKED_MEAT, 3)),
        ]));
        press(&mut app, KeyCode::Digit2, "2");
        app.update();
        assert_eq!(*app.world().resource::<SelectedSlot>(), SelectedSlot(1));
        let before = app.world().resource::<Inventory>().clone();

        let (left, played) = consume_frame(
            &mut app,
            &sent,
            [key_event(consume_key(), ButtonState::Pressed, false)],
        );

        assert_eq!(left, vec![Asked::Consume { slot: 1 }]);
        assert_eq!(played, 1, "the hand was told once per request that left");
        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "the press moved a count this client does not own"
        );
        assert_eq!(
            app.world().resource::<PickedStack>().slot(),
            None,
            "eating in play picked a source for a move nobody started"
        );
    }

    /// **A held key is one press here too**, which is the rule `player/loot.rs` states and
    /// the property `ui/inventory.rs` already pins for the pack's own consume press.
    ///
    /// The same reasoning applies unchanged and so does the same risk: what makes a repeat
    /// harmless is `bevy_input`'s `press()`, a dependency's guarantee, and this path is a
    /// second place it now has to hold. A stack the player is standing on is exactly what a
    /// per-frame repeat would eat.
    #[test]
    fn holding_the_consume_key_in_play_asks_once_and_a_later_press_asks_again() {
        let key = consume_key();
        let (mut app, sent) = hotbar_app(pack(&[(0, stack(ITEM_COOKED_MEAT, 9))]));
        let mut left = Vec::new();

        let (asked, played) = consume_frame(
            &mut app,
            &sent,
            [key_event(key, ButtonState::Pressed, false)],
        );
        left.extend(asked);
        assert_eq!(played, 1);
        // The repeats winit sends while the key is down, then a frame carrying no event at
        // all — the same held key on a machine with key repeat switched off.
        for _ in 0..3 {
            let (asked, played) = consume_frame(
                &mut app,
                &sent,
                [key_event(key, ButtonState::Pressed, true)],
            );
            left.extend(asked);
            assert_eq!(played, 0, "a repeat played the hand again");
            assert!(
                app.world().resource::<ButtonInput<KeyCode>>().pressed(key),
                "a repeat is not a release"
            );
        }
        let (asked, _) = consume_frame(&mut app, &sent, []);
        left.extend(asked);
        assert_eq!(
            left,
            vec![Asked::Consume { slot: 0 }],
            "a held key ate the stack frame by frame"
        );

        // Release and press again, so this cannot pass by the key having stopped working.
        let (asked, _) = consume_frame(
            &mut app,
            &sent,
            [key_event(key, ButtonState::Released, false)],
        );
        left.extend(asked);
        let (asked, played) = consume_frame(
            &mut app,
            &sent,
            [key_event(key, ButtonState::Pressed, false)],
        );
        left.extend(asked);
        assert_eq!(played, 1);
        assert_eq!(
            left,
            vec![Asked::Consume { slot: 0 }, Asked::Consume { slot: 0 }],
            "a press after a release is a second press"
        );
    }

    /// **Every state the press must stay silent in, and the hand stays still in all of them.**
    ///
    /// Two families in one table because they fail the same way — a frame that should not
    /// have left, and an arc that should not have played — and because the modes are the half
    /// most likely to rot: `InputMode` gains a variant far more often than the courtesy
    /// checks change. [`InputGate::may_act`] is what answers the mode question, so this sweeps
    /// the enum rather than restating a list the system does not contain.
    #[test]
    fn the_consume_key_in_play_asks_for_nothing_it_should_not() {
        let food = pack(&[(0, stack(ITEM_COOKED_MEAT, 3))]);
        for (name, stacks, dead, mode) in [
            ("an empty slot", pack(&[]), false, InputMode::Playing),
            (
                "a slot the client does not route as food",
                pack(&[(0, stack(ITEM_STONE, 4))]),
                false,
                InputMode::Playing,
            ),
            (
                "a stack of food the server has already emptied",
                pack(&[(0, stack(ITEM_COOKED_MEAT, 0))]),
                false,
                InputMode::Playing,
            ),
            ("a dead player", food.clone(), true, InputMode::Playing),
            // The pack's own press still works and is tested above; what must not happen is
            // this second reader answering the same key while that screen owns it.
            ("the pack", food.clone(), false, InputMode::Inventory),
            ("chat", food.clone(), false, InputMode::Chat),
            ("the pause menu", food.clone(), false, InputMode::Menu),
            ("a corpse", food.clone(), false, InputMode::Loot),
            ("a stall", food.clone(), false, InputMode::Vendor),
            ("the map", food.clone(), false, InputMode::Map),
        ] {
            let (mut app, sent) = hotbar_app(stacks);
            say_dead(&mut app, dead);
            *app.world_mut().resource_mut::<InputMode>() = mode;
            // A second frame, so the mode is no longer *changed* and `may_act`'s transition
            // term is not what this row is passing on.
            app.update();
            app.update();
            let _ = asked(&sent);
            app.world_mut()
                .resource_mut::<Messages<ConsumeSent>>()
                .drain()
                .for_each(drop);

            let (left, played) = consume_frame(
                &mut app,
                &sent,
                [key_event(consume_key(), ButtonState::Pressed, false)],
            );

            assert!(left.is_empty(), "{name} asked the server to eat: {left:?}");
            assert_eq!(played, 0, "{name} played the eating arc");
        }
    }

    /// **Third person closes it, exactly as it closes every other request.**
    ///
    /// Split from the table above because it is not a mode: [`InputGate::may_act`] carries the
    /// view term independently, and a press that still ate with the camera behind the
    /// character would be the one gate in this client that a view change does not close.
    #[test]
    fn the_consume_key_asks_for_nothing_from_behind_the_character() {
        let (mut app, sent) = hotbar_app(pack(&[(0, stack(ITEM_COOKED_MEAT, 3))]));
        *app.world_mut().resource_mut::<ViewMode>() = ViewMode::ThirdPerson;
        app.update();

        let (left, played) = consume_frame(
            &mut app,
            &sent,
            [key_event(consume_key(), ButtonState::Pressed, false)],
        );

        assert!(left.is_empty(), "{left:?}");
        assert_eq!(played, 0);
    }

    /// **One predicate for both spellings**, which is the point of reusing
    /// [`consume_request`] rather than writing a second one beside the hotbar.
    ///
    /// Read as a routing question the two paths ask identically: whatever [`FOODS`] contains,
    /// a cell press in the pack and this key in play agree about it, so the table cannot
    /// acquire a second copy that drifts. A new food is one entry and both paths gain it.
    #[test]
    fn the_hotbar_and_the_pack_route_the_same_press_through_one_predicate() {
        for item_id in FOODS.iter().copied().chain([ITEM_STONE, ITEM_IRON_SWORD]) {
            let stacks = pack(&[(0, stack(item_id, 2))]);
            let inventory = Inventory::from_stacks(stacks.clone());
            let routed = consume_request(&inventory, 0, 0, 4).is_some();
            assert_eq!(
                routed,
                item_is_food(item_id),
                "item {item_id} is routed by the table and not by the caller"
            );

            let (mut app, sent) = hotbar_app(stacks);
            let (left, _) = consume_frame(
                &mut app,
                &sent,
                [key_event(consume_key(), ButtonState::Pressed, false)],
            );
            assert_eq!(
                left,
                if routed {
                    vec![Asked::Consume { slot: 0 }]
                } else {
                    Vec::new()
                },
                "the hotbar press disagreed with `consume_request` about item {item_id}"
            );
        }
    }
}
