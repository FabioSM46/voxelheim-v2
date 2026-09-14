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
use super::inventory::{ApplyInventory, Inventory, MAIN_HAND_OFFSET, SelectedSlot, equipment_slot};
use super::{
    ApplySnapshots, InputCadence, InputGate, LocalMount, SelfVitals, ViewMode, set_if_changed,
};
use crate::net::{AttackRequest, Outbound, Sent, Session, encode_attack_request};
use crate::net::{BlockRequest, ConnectionState, encode_block_request};
use crate::settings::{Control, Settings};
use crate::ui::{EnergyRefused, PlayerMessage, PlayerMessageKind, PublishPlayerMessages};

/// The button that swings, and the same one that mines. Which of the two it means is
/// what [`WeaponDrawn`] decides: drawn it swings the main hand, sheathed it mines.
const SWING_BUTTON: MouseButton = MouseButton::Left;
const BLOCK_BUTTON: MouseButton = MouseButton::Right;

#[derive(Resource, Debug, Default)]
struct BlockIntent {
    raised: bool,
}

/// Item id 7, the rusty sword, as `server/internal/game/items.go` appends it.
///
/// Presentation and routing only. It cannot make another item attack-capable and it
/// cannot make this one legal: the server reads its own registry, and a swing naming a
/// slot of stone is refused there whatever this constant says.
pub(super) const ITEM_RUSTY_SWORD: u16 = 7;

/// Every item id this client routes the left button to an attack for.
///
/// **A table rather than a comparison, and that is the whole of the change.** This used to
/// be `item_id == ITEM_RUSTY_SWORD` — one weapon's name spelled inside the routing — which
/// is exactly what `armedWithSwordLocked` stopped doing on the server when it became
/// `armedForAttackLocked` and began reading weapon behavior out of the item registry.
///
/// It stays this client's own opinion and it still decides nothing: the server re-reads its
/// own registry for every swing, so a wrong entry costs a request that is refused and can
/// never grant a blow. The failure that actually cost something was the other direction —
/// an item the server would have honoured and this list omitted, which is what the iron
/// sword silently was: drawn as a blade, worth 40 damage server-side, and never asked for.
///
/// `BLADE_SHAPES` is narrower and test-only: it pins blade presentation to this routing,
/// while the bow is deliberately routed here without pretending to be a blade.
#[cfg(test)]
const BLADE_SHAPES: &[u16] = &[ITEM_RUSTY_SWORD, crafting::ITEM_IRON_SWORD];
const LEFT_BUTTON_USES: &[u16] = &[
    ITEM_RUSTY_SWORD,
    crafting::ITEM_IRON_SWORD,
    crafting::ITEM_BOW,
    crafting::ITEM_WOODEN_SCEPTRE,
];

/// Whether this client presents one item as a blade.
///
/// Test-only because runtime routing asks [`attack_item_in_hand`]; this narrower predicate
/// lets the sweep below compare the hand's blade vocabulary without inventing a stack.
#[cfg(test)]
pub(super) fn item_is_a_blade(item_id: u16) -> bool {
    BLADE_SHAPES.contains(&item_id)
}

/// Whether the main-hand weapon is drawn: the one bit that routes the left button.
///
/// **Drawn**, the left button swings the main-hand weapon and names its slot, whatever the
/// hotbar has selected, and nothing is mined or placed. **Sheathed**, the hotbar is what the
/// hand holds: the left button mines, the right button places, and a weapon selected there
/// swings nothing — the server answers only a swing that names the main hand (#1236).
///
/// **Local input routing and presentation, and nothing more** (#1239). It is never encoded
/// into any frame and it decides no outcome: a modified client that drew nothing could still
/// name the main hand, and the server would judge that swing exactly as it judges this one.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct WeaponDrawn(pub(super) bool);

/// What one frame does to [`WeaponDrawn`], and the hint a refused draw shows.
///
/// A function of four facts rather than a system body, so every combination is a row in a
/// table test. A press toggles; drawing needs something in the main hand and a player on
/// foot, and either missing leaves the weapon sheathed with a short line saying why. Past the
/// press, a drawn weapon is sheathed the frame it stops being drawable — mounting, or the
/// main hand emptied by a move, a drop or a break — so a player is never left unable to
/// mine behind a weapon that is no longer there.
fn next_drawn(
    drawn: bool,
    pressed: bool,
    mounted: bool,
    main_hand_holds: bool,
) -> (bool, Option<&'static str>) {
    let (wanted, hint) = match (pressed, drawn) {
        (true, true) => (false, None),
        (true, false) if mounted => (false, Some("Dismount to draw your weapon.")),
        (true, false) if !main_hand_holds => (
            false,
            Some("Your main hand is empty; equip a weapon to draw it."),
        ),
        (true, false) => (true, None),
        (false, drawn) => (drawn, None),
    };
    (wanted && !mounted && main_hand_holds, hint)
}

/// Orders what the hand draws after this frame's [`WeaponDrawn`] was decided, so the view
/// model and the local body change on the frame the key was pressed.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ApplyWeaponDrawn;

/// **Which slot the hand holds**: the main hand while the weapon is drawn, the selected
/// hotbar slot while sheathed.
///
/// The one answer both renderers read — the first-person view model in `super::hands` and
/// the local third-person body in `super` — so the two can never hold different things, and
/// the same split [`HeldItem`] makes for the buttons. `None` only when drawn and the session
/// announces no main hand, which draws an empty hand rather than a guessed slot.
pub(super) fn hand_slot(drawn: bool, selected: u8, session: Option<&Session>) -> Option<u8> {
    if drawn {
        session.and_then(|session| equipment_slot(&session.0, MAIN_HAND_OFFSET))
    } else {
        Some(selected)
    }
}

/// Everything the draw key reads, in one bundle.
#[derive(SystemParam)]
struct DrawIntent<'w> {
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    settings: Option<Res<'w, Settings>>,
    gate: InputGate<'w>,
    session: Option<Res<'w, Session>>,
    inventory: Res<'w, Inventory>,
    mount: Res<'w, LocalMount>,
}

/// Turns the draw key into [`WeaponDrawn`], and sheathes a weapon that can no longer be drawn.
///
/// One press is one toggle: `just_pressed` is an edge. [`InputGate::may_act`] closes the
/// press for every screen and for death, as it does for the consume key; the forced sheathe
/// below it is not a press and is not gated, so opening the pack while mounting still ends
/// the frame sheathed. Selecting a hotbar slot sheathes too, and `super::inventory` does that
/// where the selection is made.
fn draw_or_sheathe_weapon(
    intent: DrawIntent<'_>,
    mut drawn: ResMut<WeaponDrawn>,
    mut messages: MessageWriter<PlayerMessage>,
) {
    let bindings = intent
        .settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());
    let pressed = intent.gate.may_act()
        && intent
            .keys
            .is_some_and(|keys| keys.just_pressed(bindings.key(Control::DrawWeapon)));
    let main_hand_holds = intent
        .session
        .as_deref()
        .and_then(|session| equipment_slot(&session.0, MAIN_HAND_OFFSET))
        .and_then(|slot| intent.inventory.slot(slot))
        .is_some_and(|stack| stack.count > 0);
    let (next, hint) = next_drawn(drawn.0, pressed, intent.mount.mounted(), main_hand_holds);
    set_if_changed(&mut drawn, WeaponDrawn(next));
    if let Some(hint) = hint {
        messages.write(PlayerMessage::new(PlayerMessageKind::Warn, hint));
    }
}

pub(super) struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        // `PlayerCameraPlugin` owns it in the game; here too, so `InputGate` — which
        // reads it — resolves when this module is built on its own.
        app.init_resource::<ViewMode>();
        app.init_resource::<BlockIntent>()
            .init_resource::<SelfVitals>()
            .init_resource::<EnergyAnswers>()
            .init_resource::<WeaponDrawn>()
            .init_resource::<LocalMount>()
            .add_message::<SwingSent>()
            .add_message::<SwingAbandoned>()
            // `ui/status.rs` writes it in the game; registered here too so this module
            // stands up without the UI.
            .add_message::<EnergyRefused>()
            .add_message::<PlayerMessage>()
            .add_systems(
                Update,
                // Before every sender that asks [`HeldItem`], so a press of the draw key and a
                // click on one frame route the click by the state the key just chose. After the
                // snapshots and the pack, because mounting and an emptied main hand sheathe.
                draw_or_sheathe_weapon
                    .in_set(ApplyWeaponDrawn)
                    .in_set(PublishPlayerMessages)
                    .after(ApplySnapshots)
                    .after(ApplyInventory)
                    .before(ApplyCombatInput)
                    .before(super::target::ApplyTargetInput)
                    .before(super::structures::AimStructures)
                    .before(super::structures::PreviewFootprint),
            )
            .add_systems(
                Update,
                // Before both senders, so a refusal is attributed against the requests that
                // had left when it was sent — never against a press made on this frame.
                abandon_refused_swings
                    .in_set(ApplyCombatInput)
                    .before(send_attacks)
                    .before(send_block_edges),
            )
            .add_systems(
                Update,
                send_attacks
                    .in_set(ApplyCombatInput)
                    .in_set(PublishPlayerMessages)
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
                    .after(super::send_player_input)
                    // After the shield's edge, pinned rather than left to the scheduler: the
                    // two systems queue their frames in the order they run, so this order is
                    // also the order the server answers them in, and [`EnergyAnswers`] has to
                    // count a raise and a swing pressed on one frame in exactly that order.
                    .after(send_block_edges),
            )
            .add_systems(
                Update,
                send_block_edges
                    .in_set(ApplyCombatInput)
                    .in_set(PublishPlayerMessages)
                    .after(ApplySnapshots)
                    .after(super::send_player_input),
            );
    }
}

/// Sends right-button edges and lowers a local guard when gameplay input closes.
#[allow(
    clippy::too_many_arguments,
    reason = "one intent-sending system; the energy attribution needs an eighth parameter"
)]
fn send_block_edges(
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    gate: InputGate<'_>,
    cadence: Res<InputCadence>,
    state: Option<Res<ConnectionState>>,
    mut intent: ResMut<BlockIntent>,
    outbound: Option<ResMut<Outbound>>,
    mut messages: MessageWriter<PlayerMessage>,
    mut answers: ResMut<EnergyAnswers>,
) {
    let pressed = buttons
        .as_deref()
        .is_some_and(|buttons| buttons.pressed(BLOCK_BUTTON));
    let just_pressed = buttons
        .as_deref()
        .is_some_and(|buttons| buttons.just_pressed(BLOCK_BUTTON));
    let connected = state
        .as_deref()
        .is_some_and(|state| matches!(state, ConnectionState::Connected));
    let desired = gate.may_act() && connected && pressed;

    let active = if !intent.raised && desired && just_pressed {
        true
    } else if intent.raised && !desired {
        false
    } else {
        return;
    };

    let sent = outbound.map_or(Sent::Closed, |mut outbound| {
        outbound.send(encode_block_request(&BlockRequest {
            active,
            client_tick: cadence.client_tick,
        }))
    });
    if sent == Sent::Dropped {
        warn!(
            "the outbound queue was full; a shield block active={active} never reached the server"
        );
        let text = if active {
            "Raising your shield did not reach the server; try again."
        } else {
            "Lowering your shield did not reach the server; try again."
        };
        messages.write(PlayerMessage::new(PlayerMessageKind::Error, text));
    }
    if active {
        intent.raised = sent == Sent::Queued;
        if sent == Sent::Queued {
            answers.raise_sent();
        }
    } else {
        intent.raised = false;
    }
}

/// Whether the energy refusals that have arrived prove the newest swing was one of them.
///
/// **The server answers a starved swing and a starved shield raise with the same pair**,
/// `ActionRefused{Energy, NotEnoughEnergy}` (`attackRefusal` and `blockRefusal` in the
/// session), and an admitted request is answered by nothing at all. So a refusal names no
/// request. What the client does know is the order it asked in, and the session answers on
/// the goroutine that reads the requests, in arrival order.
///
/// **So it counts, rather than remembering one slot.** Every refusal read since the newest
/// swing left is set against the shield raises that left after it: each such raise can
/// explain at most one refusal, so the first refusal beyond them belongs to the swing — or to
/// something older than it, which asked the same reserve first and was refused first. That
/// settles both interleavings of a swing followed by a raise, which a single slot could only
/// settle one of (#1266):
///
///   - the swing landed and the raise after it was refused: one refusal against one raise,
///     and the swing plays in full;
///   - both were refused — the expected case, since both cost the same 25 — two refusals
///     against one raise, and the swing is abandoned when the second arrives.
///
/// A raise the server answers in some other way — admitted, or dropped because it was not a
/// new press there — only raises the bar, which is the direction a presentation may fail in:
/// a swing kept on screen, never a landed one taken back.
///
/// It decides nothing about energy. It holds no reserve and no cost; it only compares how
/// many answers arrived with how many of this client's own requests could have earned them.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct EnergyAnswers {
    /// The newest swing that left, until a refusal has been attributed to it.
    swing_unanswered: bool,
    /// Shield raises queued since that swing, each able to explain one refusal.
    raises_since_swing: u32,
    /// Energy refusals read since that swing.
    refusals_since_swing: u32,
}

impl EnergyAnswers {
    /// A swing left: everything counted so far answers something older.
    fn swing_sent(&mut self) {
        *self = Self {
            swing_unanswered: true,
            ..Self::default()
        };
    }

    /// A shield raise left, after the newest swing.
    fn raise_sent(&mut self) {
        self.raises_since_swing = self.raises_since_swing.saturating_add(1);
    }

    /// One energy refusal arrived. `true` exactly once per swing: when it proves that swing
    /// was refused.
    fn refused(&mut self) -> bool {
        self.refusals_since_swing = self.refusals_since_swing.saturating_add(1);
        if self.swing_unanswered && self.refusals_since_swing > self.raises_since_swing {
            self.swing_unanswered = false;
            return true;
        }
        false
    }
}

/// The server refused the most recent swing for energy: stop presenting it as a strike.
///
/// Read by the view-model arc and the whoosh, the two things [`SwingSent`] started. Only an
/// arc that is still playing can be abandoned — a refusal that outlives its swing has
/// nothing left on screen to take back, and the energy bar's flash answers it either way.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SwingAbandoned;

/// Turns an energy refusal that answers a swing into [`SwingAbandoned`].
///
/// **A swing is abandoned at most once**, however many refusals follow it: the ones after
/// the first that settled it answer something already taken back. See [`EnergyAnswers`] for
/// which refusal settles it.
///
/// Two outcomes are left alone on purpose, and both are the direction a presentation may
/// fail in. A swing the server admitted is never answered, so its arc plays out. A swing
/// the server drops in silence — a cooldown, a shield raised on the tick, a slot that is not
/// the main hand — is not answered either, and it plays out too: from here a silent drop and
/// a landed blow look exactly alike, and only a refusal is evidence of anything.
fn abandon_refused_swings(
    mut refusals: MessageReader<EnergyRefused>,
    mut answers: ResMut<EnergyAnswers>,
    mut abandoned: MessageWriter<SwingAbandoned>,
) {
    for _ in refusals.read() {
        if answers.refused() {
            abandoned.write(SwingAbandoned);
        }
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
/// The item id is presentation-only: `super::hands` uses it to select a blade arc or bow
/// draw. The `AttackRequest` below still names only a slot and tick, so the picture cannot
/// decide what the authoritative slot does.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SwingSent {
    /// Presentation only: the server still receives a slot and resolves its contents.
    pub(super) item_id: u16,
}

/// Whether the selected slot presents a usable blade in tests.
///
/// A blade worn through is not one: the server refuses its attack, so the presentation
/// helper mirrors the same usable-stack rule as [`attack_item_in_hand`].
///
/// **Worn through means zero under a non-zero maximum**, which is the pair
/// `armedForAttackLocked` reads and not the current value on its own. `max_durability > 0`
/// is already this client's answer to *does this wear out* — `super::inventory`'s
/// `repair_request` asks it in exactly that shape — and a weapon that never wears out would
/// carry `(0, 0)` like every resource does. Reading the current value alone would call such
/// a weapon permanently broken the day somebody registered one, which is a courtesy refusing
/// a swing the server would have granted: the one direction a courtesy must never fail in.
#[cfg(test)]
pub(super) fn blade_in_hand(inventory: &Inventory, selected: &SelectedSlot) -> bool {
    inventory.slot(selected.0).is_some_and(|stack| {
        item_is_a_blade(stack.item_id)
            && stack.count > 0
            && (stack.max_durability == 0 || stack.durability > 0)
    })
}

/// The attack-capable presentation item one slot holds, if it is present and usable.
/// Ammunition is deliberately absent: only the server decides whether a bow may fire.
pub(super) fn attack_item_in_hand(inventory: &Inventory, slot: u8) -> Option<u16> {
    inventory.slot(slot).and_then(|stack| {
        (LEFT_BUTTON_USES.contains(&stack.item_id)
            && stack.count > 0
            && (stack.max_durability == 0 || stack.durability > 0))
            .then_some(stack.item_id)
    })
}

/// What the hand holds and which slot each button names, as the input systems ask about it.
///
/// A bundle rather than separate parameters, because every system that routes a button
/// needs the same facts and `send_block_edits` was already at the argument bound. Bundling
/// also puts the shared questions — does the left button swing, mine, or neither; does the
/// right button place — on one type instead of leaving call sites to ask them the same way.
///
/// **Two hands, one bit between them** ([`WeaponDrawn`]). The swing reads the main hand while
/// drawn; mining and placing read the hotbar while sheathed. Neither ever reads the other.
#[derive(SystemParam)]
pub(super) struct HeldItem<'w> {
    inventory: Res<'w, Inventory>,
    selected: Res<'w, SelectedSlot>,
    session: Option<Res<'w, Session>>,
    drawn: Res<'w, WeaponDrawn>,
}

impl HeldItem<'_> {
    /// Which authoritative slot the hotbar has selected: the one mining and placing name.
    pub(super) fn slot(&self) -> u8 {
        self.selected.0
    }

    /// The main-hand slot and the usable attack item in it, while the weapon is drawn.
    ///
    /// `None` while sheathed, whatever the hotbar holds: a swing names only the main hand,
    /// found through [`equipment_slot`] and never as a literal.
    pub(super) fn swing(&self) -> Option<(u8, u16)> {
        if !self.drawn.0 {
            return None;
        }
        let slot = equipment_slot(&self.session.as_deref()?.0, MAIN_HAND_OFFSET)?;
        attack_item_in_hand(&self.inventory, slot).map(|item_id| (slot, item_id))
    }

    /// Which usable attack item a left press would swing, if any. See [`Self::swing`].
    pub(super) fn attack_item(&self) -> Option<u16> {
        self.swing().map(|(_, item_id)| item_id)
    }

    /// Whether the left button mines: sheathed, and no weapon selected on the hotbar.
    ///
    /// A hotbar weapon mines nothing and swings nothing. It never mined before the main hand
    /// existed, and a press that asked for neither is the honest answer to a weapon held
    /// where the server will not swing it. A worn-through one mines, as it always has.
    pub(super) fn left_button_mines(&self) -> bool {
        !self.drawn.0 && attack_item_in_hand(&self.inventory, self.selected.0).is_none()
    }

    /// Whether the right button may place a block or a structure: only while sheathed. Drawn,
    /// it raises a worn shield and nothing else.
    pub(super) fn places(&self) -> bool {
        !self.drawn.0
    }

    /// Which structure that slot would plant, if it plants one — never while drawn.
    ///
    /// Read by the placement sender in [`super::structures`] and by the block-edit path in
    /// [`super::target`], for the reason [`Self::attack_item`] is read by two sites: one press
    /// must never ask for a voxel and a building at once, and asking the same function is
    /// what makes that structural.
    pub(super) fn structure(&self) -> Option<crate::net::StructureKind> {
        if !self.places() {
            return None;
        }
        super::structures::structure_in_hand(
            self.inventory
                .slot(self.selected.0)
                .filter(|stack| stack.count > 0)
                .map(|stack| stack.item_id),
        )
    }
}

/// Sends exactly one `AttackRequest` per press, while a usable weapon is drawn.
///
/// **The slot is the main hand's**, whatever the hotbar has selected, and the swing's shape
/// follows the main-hand item. Sheathed, a press sends nothing from here. A swing from the
/// main hand is still a swing, so [`EnergyAnswers`] counts it exactly as before. A drawn bow
/// sends an `AttackRequest` the server ignores — its bow fires only through a draw (#1237).
///
/// `just_pressed`, never `pressed`: a swing is an event and the server refuses a second
/// one inside its cooldown anyway, so holding the button down would only fill the
/// outbound queue with frames that are declined on arrival.
///
/// **A press behind a raised shield starts no swing** (#1228). The server drops every swing
/// while it says this player is blocking, before it creates anything, so a press there is
/// neither sent nor animated. Both halves of the test are needed: the server's `blocking` is
/// the fact, and the local raise still being held is what keeps a stale one from swallowing
/// the counter-strike a player makes the frame after lowering the shield — that release left
/// before this press, so the server lowers the shield before it judges the swing.
#[allow(
    clippy::too_many_arguments,
    reason = "one intent-sending system; the chat report, the shield and the energy attribution need the rest"
)]
fn send_attacks(
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    gate: InputGate<'_>,
    held: HeldItem<'_>,
    cadence: Res<InputCadence>,
    outbound: Option<ResMut<Outbound>>,
    structure: Res<super::structures::StructureTarget>,
    mut swings: MessageWriter<SwingSent>,
    mut messages: MessageWriter<PlayerMessage>,
    (vitals, intent): (Res<SelfVitals>, Res<BlockIntent>),
    mut answers: ResMut<EnergyAnswers>,
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
    let Some((slot, item_id)) = held.swing() else {
        return;
    };
    if intent.raised && vitals.get().is_some_and(|vitals| vitals.blocking) {
        return;
    }

    let Some(mut outbound) = outbound else {
        return;
    };
    let request = AttackRequest {
        slot,
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
            swings.write(SwingSent { item_id });
            answers.swing_sent();
        }
        Sent::Dropped => {
            warn!(
                "the outbound queue was full; a swing from slot {} never reached the server",
                request.slot
            );
            messages.write(PlayerMessage::new(
                PlayerMessageKind::Error,
                "Your attack did not reach the server; try again.",
            ));
        }
        // The session is ending. There is nowhere to send and nothing to celebrate.
        Sent::Closed => {}
    }
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<BlockIntent>(world);
    crate::world::transition::reset::<EnergyAnswers>(world);
    crate::world::transition::reset::<WeaponDrawn>(world);
}

/// Presses the default [`Control::DrawWeapon`] key the way a window does, for tests in any
/// player module. Needs `InputPlugin`; release is not required between presses of different
/// keys, but is between two presses of this one.
#[cfg(test)]
pub(super) fn press_draw_weapon(app: &mut App, state: bevy::input::ButtonState) {
    use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
    app.world_mut().write_message(KeyboardInput {
        key_code: crate::settings::Bindings::default().key(Control::DrawWeapon),
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state,
        text: None,
        repeat: false,
        window: Entity::PLACEHOLDER,
    });
}

#[cfg(test)]
mod tests {
    //! No window, no display and no GPU. What is asserted is the bytes that left, because
    //! the frame is what the server acts on.

    use std::sync::mpsc::Receiver;

    use bevy::asset::AssetPlugin;
    use bevy::ecs::message::MessageCursor;
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
            inventory_slots: 41,
            hotbar_slots: 9,
            equipment_slots: 5,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    /// The main hand's absolute slot in [`session`], through the one accessor.
    fn main_hand() -> u8 {
        equipment_slot(&session().0, MAIN_HAND_OFFSET).expect("the session has a main hand")
    }

    /// Presses and lets go of the draw key, a frame each.
    fn toggle_draw(app: &mut App) {
        press_draw_weapon(app, ButtonState::Pressed);
        app.update();
        press_draw_weapon(app, ButtonState::Released);
        app.update();
    }

    fn drawn(app: &App) -> bool {
        app.world().resource::<WeaponDrawn>().0
    }

    /// An app that can click, somewhere for the frames to go, and `main_hand` in the main hand
    /// — drawn when there is something to draw.
    ///
    /// The queue is deeper than any of these tests needs, so a full one can never be what
    /// makes a request go missing.
    fn clicking_app(main_hand: InventoryStack) -> (App, Receiver<Vec<u8>>) {
        let (mut app, sent) = sheathed_app(&[(self::main_hand(), main_hand)]);
        if main_hand.count > 0 {
            toggle_draw(&mut app);
            assert!(drawn(&app), "the main-hand weapon did not draw");
        }
        drain(&sent);
        (app, sent)
    }

    /// An app holding `pack`, with nothing drawn.
    fn sheathed_app(pack: &[(u8, InventoryStack)]) -> (App, Receiver<Vec<u8>>) {
        let mut app = App::new();
        let (outbound, sent) = Outbound::to_a_test(64);
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), InputPlugin))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(ConnectionState::Connected)
            .insert_resource(outbound)
            .add_plugins(PlayerPlugin);

        deliver_pack(&mut app, pack);
        // Two frames: the first ingests the pack, and the second leaves `InputMode` unchanged
        // so `may_act` is not looking at a mode that changed this frame.
        app.update();
        app.update();
        drain(&sent);
        (app, sent)
    }

    /// [`clicking_app`], with an outbound queue that has no room for anything.
    ///
    /// A zero-capacity `sync_channel` is a rendezvous: `try_send` succeeds only while a
    /// receiver is parked in a blocking `recv`, which nothing here ever is, so every send
    /// reads as `Sent::Dropped`. The receiver is returned rather than discarded — dropping
    /// it would disconnect the channel and turn every later send into the silent
    /// `Sent::Closed` instead.
    fn clicking_app_that_drops(main_hand: InventoryStack) -> (App, Receiver<Vec<u8>>) {
        let mut app = App::new();
        let (outbound, never_received) = Outbound::to_a_test(0);
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), InputPlugin))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(ConnectionState::Connected)
            .insert_resource(outbound)
            .add_plugins(PlayerPlugin);

        deliver(&mut app, main_hand);
        app.update();
        app.update();
        if main_hand.count > 0 {
            toggle_draw(&mut app);
        }
        (app, never_received)
    }

    fn player_messages(app: &App) -> Vec<PlayerMessage> {
        let messages = app.world().resource::<Messages<PlayerMessage>>();
        let mut cursor = messages.get_cursor();
        cursor.read(messages).cloned().collect()
    }

    #[test]
    fn a_dropped_swing_reaches_chat_as_one_error() {
        let (mut app, _never_received) = clicking_app_that_drops(blade());
        click(&mut app);
        app.update();

        assert_eq!(
            player_messages(&app),
            [PlayerMessage::new(
                PlayerMessageKind::Error,
                "Your attack did not reach the server; try again."
            )]
        );
    }

    #[test]
    fn a_dropped_shield_raise_reaches_chat_as_one_error() {
        let (mut app, _never_received) = clicking_app_that_drops(InventoryStack::default());
        block_button(&mut app, ButtonState::Pressed);
        app.update();

        assert_eq!(
            player_messages(&app),
            [PlayerMessage::new(
                PlayerMessageKind::Error,
                "Raising your shield did not reach the server; try again."
            )]
        );
    }

    /// Replaces the pack wholesale with `main_hand` in the main hand, as one more complete
    /// `InventoryState` from the server.
    ///
    /// Whole rather than edited, because that is the only kind of inventory this client
    /// has: there is no `set_slot` here to reach for.
    fn deliver(app: &mut App, main_hand: InventoryStack) {
        deliver_pack(app, &[(self::main_hand(), main_hand)]);
    }

    /// Replaces the pack wholesale with these stacks at these slots.
    fn deliver_pack(app: &mut App, pack: &[(u8, InventoryStack)]) {
        let mut stacks = vec![InventoryStack::default(); usize::from(session().0.inventory_slots)];
        for (slot, stack) in pack {
            stacks[usize::from(*slot)] = *stack;
        }
        app.world_mut()
            .resource_mut::<InventoryInbox>()
            .push(InventoryState { stacks, silver: 0 });
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

    fn block_button(app: &mut App, state: ButtonState) {
        app.world_mut().write_message(MouseButtonInput {
            button: BLOCK_BUTTON,
            state,
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

    fn blocks(sent: &Receiver<Vec<u8>>) -> Vec<(bool, u32)> {
        let mut found = Vec::new();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            if let Some(request) = envelope.payload_as_block_request() {
                found.push((request.active(), request.client_tick()));
            }
        }
        found
    }

    #[test]
    fn right_button_sends_one_raise_and_one_release_edge() {
        let (mut app, sent) = clicking_app(InventoryStack::default());
        block_button(&mut app, ButtonState::Pressed);
        app.update();
        let raised_tick = app.world().resource::<InputCadence>().client_tick;
        assert_eq!(blocks(&sent), vec![(true, raised_tick)]);

        for _ in 0..3 {
            app.update();
        }
        assert!(
            blocks(&sent).is_empty(),
            "holding repeated the block request"
        );

        block_button(&mut app, ButtonState::Released);
        app.update();
        let released_tick = app.world().resource::<InputCadence>().client_tick;
        assert_eq!(blocks(&sent), vec![(false, released_tick)]);
    }

    #[test]
    fn a_ui_transition_releases_a_locally_raised_shield_once() {
        let (mut app, sent) = clicking_app(InventoryStack::default());
        block_button(&mut app, ButtonState::Pressed);
        app.update();
        drain(&sent);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();
        assert_eq!(
            blocks(&sent).iter().map(|edge| edge.0).collect::<Vec<_>>(),
            vec![false]
        );
        app.update();
        assert!(blocks(&sent).is_empty(), "the automatic release repeated");
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
            assert_eq!(
                found[0].0,
                main_hand(),
                "{name}: the swing named the wrong slot"
            );
        }
    }

    /// Local ammunition is never a gate: the server owns both the count and the refusal.
    ///
    /// From the drawn main hand, where the bow now has to be for a press to name it. The
    /// server launches nothing from this frame since #1237 — its bow fires through a draw —
    /// and that is its answer to give, not a reason to stop asking here.
    #[test]
    fn one_click_with_a_bow_sends_an_attack_without_checking_for_arrows() {
        let (mut app, sent) = clicking_app(blade_of(crafting::ITEM_BOW));
        click(&mut app);
        app.update();

        assert_eq!(attacks(&sent).len(), 1);
        let messages = app.world().resource::<Messages<SwingSent>>();
        assert_eq!(messages.len(), 1, "the sent bow attack did not animate");
    }

    #[test]
    fn one_click_with_a_sceptre_sends_an_attack() {
        let (mut app, sent) = clicking_app(blade_of(crafting::ITEM_WOODEN_SCEPTRE));
        click(&mut app);
        app.update();

        assert_eq!(attacks(&sent).len(), 1);
        let messages = app.world().resource::<Messages<SwingSent>>();
        assert_eq!(messages.len(), 1, "the sent sceptre cast did not animate");
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
            assert_eq!(attacks(&sent), vec![(main_hand(), tick)], "{name}");
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
                        blocking: false,
                        energy: 100,
                        max_energy: 100,
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

        deliver(&mut app, blade());
        app.update();
        app.update();
        toggle_draw(&mut app);
        assert!(
            drawn(&app),
            "the blade did not draw, so this test proves nothing"
        );

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

    /// Every [`SwingAbandoned`] written since `cursor` last read.
    fn abandons(app: &App, cursor: &mut MessageCursor<SwingAbandoned>) -> usize {
        cursor
            .read(app.world().resource::<Messages<SwingAbandoned>>())
            .count()
    }

    /// The pair the server answers a starved request with, as `ui/status.rs` hands it on.
    fn refuse_for_energy(app: &mut App) {
        app.world_mut().write_message(EnergyRefused);
    }

    /// A living player at full energy, with the server's word on the shield.
    fn vitals_blocking(blocking: bool) -> SelfVitals {
        SelfVitals::from_server(PlayerVitals {
            health: 100,
            max_health: 100,
            hunger: 50,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state: crate::net::LifeState::Alive,
            respawn_ticks: 0,
            invulnerable: false,
            blocking,
            energy: 100,
            max_energy: 100,
        })
    }

    /// **A swing the server refuses for energy is abandoned, once** (#1228).
    ///
    /// Every attack item, because the server charges one reserve for all of them. Silence
    /// abandons nothing — that swing may have landed — and a second refusal with nothing
    /// newer asked answers a swing already taken back.
    #[test]
    fn a_swing_refused_for_energy_is_abandoned_once() {
        for (name, stack) in [
            ("a blade", blade()),
            ("a bow", blade_of(crafting::ITEM_BOW)),
            ("a sceptre", blade_of(crafting::ITEM_WOODEN_SCEPTRE)),
        ] {
            let (mut app, sent) = clicking_app(stack);
            let mut cursor = app
                .world()
                .resource::<Messages<SwingAbandoned>>()
                .get_cursor();
            click(&mut app);
            app.update();
            app.update();
            assert_eq!(attacks(&sent).len(), 1, "{name}: the press sent no swing");
            assert_eq!(
                abandons(&app, &mut cursor),
                0,
                "{name}: a swing nobody answered was abandoned"
            );

            refuse_for_energy(&mut app);
            app.update();
            assert_eq!(
                abandons(&app, &mut cursor),
                1,
                "{name}: the refusal did not abandon the swing it answered"
            );

            refuse_for_energy(&mut app);
            app.update();
            assert_eq!(
                abandons(&app, &mut cursor),
                0,
                "{name}: a second refusal abandoned a swing already taken back"
            );
        }
    }

    /// **A swing followed by a shield raise: one refusal is the raise's, two are both** (#1266).
    ///
    /// The server answers a starved raise with `ActionRefused{Energy, NotEnoughEnergy}` too, and
    /// answers in the order the two left. One refusal therefore cannot be the swing's alone —
    /// the swing may have landed and only the raise after it been refused — so it takes nothing
    /// back. A second refusal is more than the raise can explain, so the swing was refused as
    /// well, which is the expected case when both cost the same reserve.
    #[test]
    fn a_swing_then_a_shield_raise_is_abandoned_only_when_both_are_refused() {
        let (mut app, sent) = clicking_app(blade());
        let mut cursor = app
            .world()
            .resource::<Messages<SwingAbandoned>>()
            .get_cursor();
        click(&mut app);
        app.update();
        block_button(&mut app, ButtonState::Pressed);
        app.update();
        let found = attacks(&sent);
        assert_eq!(found.len(), 1, "the press sent no swing");

        refuse_for_energy(&mut app);
        app.update();
        assert_eq!(
            abandons(&app, &mut cursor),
            0,
            "the refusal of the raise abandoned the swing before it"
        );

        refuse_for_energy(&mut app);
        app.update();
        assert_eq!(
            abandons(&app, &mut cursor),
            1,
            "the swing's own refusal, behind the raise's, abandoned nothing"
        );
    }

    /// The attack and shield requests waiting on the queue, in the order they left.
    fn combat_requests(sent: &Receiver<Vec<u8>>) -> Vec<&'static str> {
        let mut found = Vec::new();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            if envelope.payload_as_attack_request().is_some() {
                found.push("attack");
            } else if envelope.payload_as_block_request().is_some() {
                found.push("block");
            }
        }
        found
    }

    /// **A raise and a swing pressed on one frame leave in one pinned order** (#1266).
    ///
    /// The order the two systems run is the order their frames reach the server and the order
    /// it answers them in, so it is scheduled explicitly rather than left ambiguous. The raise
    /// leaves first, so a refusal that follows is set against the swing with no raise after
    /// it: a starved pair is abandoned on the first refusal.
    #[test]
    fn a_raise_and_a_swing_on_one_frame_leave_raise_first() {
        let (mut app, sent) = clicking_app(blade());
        let mut cursor = app
            .world()
            .resource::<Messages<SwingAbandoned>>()
            .get_cursor();
        block_button(&mut app, ButtonState::Pressed);
        click(&mut app);
        app.update();
        assert_eq!(
            combat_requests(&sent),
            ["block", "attack"],
            "the raise and the swing did not leave in the pinned order"
        );

        refuse_for_energy(&mut app);
        app.update();
        assert_eq!(
            abandons(&app, &mut cursor),
            1,
            "a refusal after a raise-then-swing did not abandon the swing"
        );
    }

    /// Every interleaving of swings, raises and refusals the attribution distinguishes.
    ///
    /// Written against the counter itself, one event at a time, because the property is about
    /// order and a frame-driven test can only reach the orders a frame produces.
    #[test]
    fn energy_answers_attribute_every_interleaving() {
        #[derive(Clone, Copy)]
        enum Step {
            Swing,
            Raise,
            Refusal,
        }
        use Step::{Raise, Refusal, Swing};

        for (name, steps, expected) in [
            (
                "a refusal with no swing",
                &[Raise, Refusal][..],
                &[false][..],
            ),
            ("a refused swing", &[Swing, Refusal][..], &[true][..]),
            (
                "a refused swing, answered twice",
                &[Swing, Refusal, Refusal][..],
                &[true, false][..],
            ),
            (
                "a swing that landed, then a refused raise",
                &[Swing, Raise, Refusal][..],
                &[false][..],
            ),
            (
                "a refused swing, then a refused raise",
                &[Swing, Raise, Refusal, Refusal][..],
                &[false, true][..],
            ),
            (
                "a refused raise, then a swing before its answer",
                &[Raise, Swing, Refusal][..],
                &[true][..],
            ),
            (
                "a swing that landed, then two refused raises",
                &[Swing, Raise, Raise, Refusal, Refusal][..],
                &[false, false][..],
            ),
            (
                "an older swing's refusal after a newer swing left",
                &[Swing, Refusal, Swing, Refusal][..],
                &[true, true][..],
            ),
        ] {
            let mut answers = EnergyAnswers::default();
            let mut settled = Vec::new();
            for step in steps {
                match step {
                    Swing => answers.swing_sent(),
                    Raise => answers.raise_sent(),
                    Refusal => settled.push(answers.refused()),
                }
            }
            assert_eq!(settled, expected, "{name}");
        }
    }

    /// **A press behind a shield the server says is up starts no swing** (#1228) — and a stale
    /// `blocking` does not swallow the counter-strike made after letting the shield go.
    #[test]
    fn a_press_behind_a_raised_shield_starts_no_swing_until_it_is_let_go() {
        let (mut app, sent) = clicking_app(blade());
        block_button(&mut app, ButtonState::Pressed);
        app.update();
        *app.world_mut().resource_mut::<SelfVitals>() = vitals_blocking(true);
        click(&mut app);
        app.update();
        assert!(
            attacks(&sent).is_empty(),
            "a press behind the raised shield sent a swing"
        );
        assert_eq!(
            app.world().resource::<Messages<SwingSent>>().len(),
            0,
            "a press behind the raised shield animated a swing"
        );

        // The shield is let go, and no snapshot has said so yet. The release left first, so the
        // server lowers the shield before it judges the next press.
        release(&mut app);
        block_button(&mut app, ButtonState::Released);
        app.update();
        click(&mut app);
        app.update();
        assert_eq!(
            attacks(&sent).len(),
            1,
            "the swing made after letting the shield go never left"
        );
        assert!(
            app.world()
                .resource::<SelfVitals>()
                .get()
                .is_some_and(|vitals| vitals.blocking),
            "the vitals stopped saying blocking, so the half above proves nothing"
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
            vec![main_hand(); 3],
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

    // -----------------------------------------------------------------------
    // Drawing the main-hand weapon (#1239)
    // -----------------------------------------------------------------------

    /// Every combination of press, mount and main hand the draw rule distinguishes.
    #[test]
    fn next_drawn_answers_every_press_and_every_reason_to_sheathe() {
        const EMPTY: Option<&str> = Some("Your main hand is empty; equip a weapon to draw it.");
        const RIDING: Option<&str> = Some("Dismount to draw your weapon.");
        // (name, drawn, pressed, mounted, main hand holds) -> (drawn, hint)
        for (name, drawn, pressed, mounted, holds, want) in [
            (
                "a press draws a held weapon",
                false,
                true,
                false,
                true,
                (true, None),
            ),
            (
                "a second press sheathes it",
                true,
                true,
                false,
                true,
                (false, None),
            ),
            (
                "an empty main hand stays sheathed",
                false,
                true,
                false,
                false,
                (false, EMPTY),
            ),
            (
                "a rider stays sheathed",
                false,
                true,
                true,
                true,
                (false, RIDING),
            ),
            (
                "no press keeps it drawn",
                true,
                false,
                false,
                true,
                (true, None),
            ),
            (
                "no press keeps it sheathed",
                false,
                false,
                false,
                true,
                (false, None),
            ),
            ("mounting sheathes", true, false, true, true, (false, None)),
            (
                "an emptied main hand sheathes",
                true,
                false,
                false,
                false,
                (false, None),
            ),
            (
                "a press while mounted and drawn sheathes",
                true,
                true,
                true,
                true,
                (false, None),
            ),
        ] {
            assert_eq!(next_drawn(drawn, pressed, mounted, holds), want, "{name}");
        }
    }

    /// **The key toggles, and the drawn state never leaves this client.**
    ///
    /// Every frame sent across three presses is the tick-paced input stream and nothing else:
    /// no frame carries the state, because no frame has a field for it.
    #[test]
    fn the_draw_key_toggles_and_encodes_nothing() {
        let (mut app, sent) = sheathed_app(&[(main_hand(), blade())]);
        let mut frames = Vec::new();
        let mut seen = Vec::new();
        for _ in 0..3 {
            toggle_draw(&mut app);
            seen.push(drawn(&app));
            while let Ok(frame) = sent.try_recv() {
                frames.push(frame);
            }
        }
        assert_eq!(seen, [true, false, true]);
        for frame in frames {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            assert!(
                envelope.payload_as_player_input().is_some(),
                "drawing or sheathing sent a {:?} frame",
                envelope.payload_type()
            );
        }
    }

    /// Pressing the key with an empty main hand stays sheathed and says why, once.
    #[test]
    fn drawing_with_an_empty_main_hand_stays_sheathed_and_hints() {
        let (mut app, sent) = sheathed_app(&[(0, blade())]);
        press_draw_weapon(&mut app, ButtonState::Pressed);
        app.update();

        assert!(!drawn(&app), "an empty main hand drew something");
        assert_eq!(
            player_messages(&app),
            [PlayerMessage::new(
                PlayerMessageKind::Warn,
                "Your main hand is empty; equip a weapon to draw it."
            )]
        );
        click(&mut app);
        app.update();
        assert!(attacks(&sent).is_empty(), "a sheathed press swung");
    }

    /// **Drawn, the press swings the main hand whatever the hotbar selects** — and the swing's
    /// shape follows the main-hand item, not the selected one.
    #[test]
    fn a_drawn_press_swings_the_main_hand_whatever_the_hotbar_selects() {
        let (mut app, sent) = sheathed_app(&[
            (0, blade_of(ITEM_IRON_SWORD)),
            (2, one(ITEM_STONE)),
            (main_hand(), blade_of(crafting::ITEM_WOODEN_SCEPTRE)),
        ]);
        // One cursor for the whole test: a message outlives the frame it was written on, so a
        // cursor made fresh for the second press would still read the first press's swing.
        let mut cursor = app.world().resource::<Messages<SwingSent>>().get_cursor();
        for selected in [0, 2] {
            // Written directly: a hotbar key would sheathe, which is its own test below.
            *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(selected);
            if !drawn(&app) {
                toggle_draw(&mut app);
            }
            click(&mut app);
            app.update();
            release(&mut app);
            app.update();

            let found = attacks(&sent);
            assert_eq!(found.len(), 1, "slot {selected}: {found:?}");
            assert_eq!(found[0].0, main_hand(), "slot {selected}");
            let swung: Vec<u16> = cursor
                .read(app.world().resource::<Messages<SwingSent>>())
                .map(|swing| swing.item_id)
                .collect();
            assert_eq!(swung, [crafting::ITEM_WOODEN_SCEPTRE], "slot {selected}");
        }
    }

    /// A weapon selected on the hotbar while sheathed does not attack: no request, no swing.
    #[test]
    fn a_weapon_on_the_hotbar_while_sheathed_neither_swings_nor_animates() {
        for (name, stack) in blades() {
            let (mut app, sent) =
                sheathed_app(&[(0, stack), (main_hand(), blade_of(ITEM_IRON_SWORD))]);
            click(&mut app);
            app.update();
            assert!(attacks(&sent).is_empty(), "{name}: a sheathed press swung");
            assert_eq!(
                app.world().resource::<Messages<SwingSent>>().len(),
                0,
                "{name}: a sheathed press animated a swing"
            );
        }
    }

    /// Selecting a hotbar slot while drawn sheathes — the slot already selected included.
    #[test]
    fn selecting_a_hotbar_slot_sheathes_the_drawn_weapon() {
        use bevy::input::keyboard::{Key, KeyboardInput};
        let (mut app, sent) = clicking_app(blade());
        assert_eq!(app.world().resource::<SelectedSlot>().0, 0);
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::Digit1,
            logical_key: Key::Character("1".into()),
            state: ButtonState::Pressed,
            text: Some("1".into()),
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert!(
            !drawn(&app),
            "selecting a hotbar slot left the weapon drawn"
        );

        click(&mut app);
        app.update();
        assert!(attacks(&sent).is_empty(), "the sheathed press still swung");
    }

    /// Mounting sheathes: the frame the server's snapshot says this session rides.
    #[test]
    fn mounting_sheathes_the_drawn_weapon() {
        use crate::net::{MountKind, MountState, Snapshot, SnapshotInbox};
        let (mut app, _sent) = clicking_app(blade());
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: 1,
                mounts: vec![MountState {
                    entity_id: session().0.entity_id,
                    mount: MountKind::GreyHorse,
                }],
                ..Default::default()
            },
            std::time::Instant::now(),
        );
        app.update();
        assert!(
            app.world().resource::<LocalMount>().mounted(),
            "the snapshot did not mount this session, so this test proves nothing"
        );
        assert!(!drawn(&app), "mounting left the weapon drawn");
    }

    /// A drawn weapon that leaves the main hand sheathes, so the left button mines again.
    #[test]
    fn a_main_hand_emptied_while_drawn_sheathes() {
        let (mut app, _sent) = clicking_app(blade());
        deliver(&mut app, InventoryStack::default());
        app.update();
        assert!(!drawn(&app), "an empty main hand stayed drawn");
    }
}
