//! Client-side presentation of one server-owned stall.
//!
//! **The mirror of `loot.rs`, and deliberately smaller than it.** A corpse is a container
//! whose contents this client watches empty; a stall is a price list, with nothing
//! consumable in it. What is left after that difference is the session: the server decides
//! which stall a player has open, says so with a `VendorState`, and ends it with a
//! `VendorClosed`. This module holds the newest list and the mode that goes with it.

use bevy::prelude::*;

use super::{ApplyInputMode, ApplySnapshots, InputCadence, InputGate, InputMode, SelfVitals};
use crate::net::{
    Outbound, Session, TradeRequest, VendorEvent, VendorInbox, VendorState, encode_trade_request,
};

/// The newest complete price list currently shown, or `None` when no stall is open.
#[derive(Resource, Debug, Default)]
pub struct VendorWindow {
    current: Option<VendorState>,
}

impl VendorWindow {
    pub fn state(&self) -> Option<&VendorState> {
        self.current.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn from_server(state: VendorState) -> Self {
        Self {
            current: Some(state),
        }
    }
}

/// A press on one row's `Buy` or `Sell` button.
///
/// **It carries what was asked for and nothing about what will happen.** No price, no
/// total, no verdict about the purse: the server owns all three, and a message that named
/// one would be this client stating an outcome. The count is the shift modifier already
/// resolved — `ui/vendor.rs` reads the key, exactly as `ui/inventory.rs` reads it for a
/// drop — because which of two amounts a press meant is a fact about the press.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorTradeClick {
    pub item_id: u16,
    /// True for a row in the vendor's `sells` list, false for one in its `buys` list.
    pub buying: bool,
    /// One, or [`SHIFT_COUNT`] while shift is held.
    pub count: u16,
}

/// What a shift-click asks for instead of one.
///
/// **All or nothing, which is the server's rule and not a courtesy this side adds.** Ten
/// arrows the purse only covers seven of buys none of them, and the refusal is what says
/// so — a client that trimmed the count to what it thought was affordable would be
/// deciding the trade.
pub const SHIFT_COUNT: u16 = 10;

pub(super) struct VendorPlugin;

impl Plugin for VendorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VendorWindow>()
            .init_resource::<VendorInbox>()
            .add_message::<VendorTradeClick>()
            .add_systems(
                Update,
                reconcile_vendor
                    .after(crate::net::DrainNetwork)
                    .after(ApplySnapshots)
                    .before(ApplyInputMode),
            )
            // After the mode has been chosen, because the press this reads is the one that
            // just changed it: `Escape` leaves `Vendor`, and the window it left has to go
            // with it in the same frame rather than one later.
            .add_systems(
                Update,
                (dismiss_the_stall_the_player_left, send_trade_intents).after(ApplyInputMode),
            );
    }
}

/// Applies the server's answers in order, and closes a window the newest one invalidated.
///
/// **There is no staleness guard here and `loot.rs` has one, which is a difference worth
/// stating rather than leaving to look like an omission.** Loot refuses a revision it has
/// already seen, because an equal one arriving late would restore an entry the player had
/// taken. A stall has no such entry: `VendorState` replaces this client's view wholesale by
/// contract, and `openVendorLocked` deliberately re-sends *the revision it already has* when
/// a player addresses the stall they already have open. A guard in loot's shape would
/// swallow that frame, and the interact key would do nothing at a stall closed with
/// `Escape`.
fn reconcile_vendor(
    mut inbox: ResMut<VendorInbox>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut window: ResMut<VendorWindow>,
    mut mode: ResMut<InputMode>,
) {
    for event in inbox.take() {
        match event {
            VendorEvent::State(state) => {
                // Whatever was open is replaced, with no closure needed for it: the server
                // closes the first stall when a second opens, but the two frames may arrive
                // in either order and there is one window to show either in.
                window.current = Some(state);
                set_mode(&mut mode, InputMode::Vendor);
            }
            VendorEvent::Closed(closed) => {
                if window
                    .current
                    .as_ref()
                    .is_some_and(|state| state.entity_id == closed.entity_id)
                {
                    close(&mut window, &mut mode);
                }
            }
        }
    }

    // Death ends the session at the stall from this side too, and so does losing the
    // session. Neither decides anything — the server refuses every trade from a corpse
    // through the act gate that refuses mining — it is a window that must not sit over a
    // death overlay showing prices nobody can pay.
    if session.is_none() || vitals.dead() {
        close(&mut window, &mut mode);
    }
}

/// Takes the window down when the mode has left it.
///
/// The one gesture this client answers on its own: `Escape` changes the mode, and the list
/// goes with it. Nothing is sent — there is no message for "I have stopped looking", by
/// contract — so the server keeps the session until the player walks away or addresses
/// somebody else, and re-addressing this stall re-sends the list it already had.
fn dismiss_the_stall_the_player_left(mode: Res<InputMode>, mut window: ResMut<VendorWindow>) {
    if window.current.is_some() && *mode != InputMode::Vendor {
        window.current = None;
    }
}

/// Takes the window down and hands the mode back, writing to neither unless it moves.
///
/// **Three guards, and none of them is an optimisation**, because this runs on every frame
/// a player is dead. `**mode == InputMode::Vendor` keeps the mode this module owns the only
/// one it leaves: death deliberately does not close the pause menu or chat, and handing the
/// mode back unconditionally would take a corpse out of the menu it is quitting from twenty
/// times a second. The other two are change detection — dereferencing a `ResMut` marks its
/// resource changed whether or not the value moved, and `InputMode`'s change flag is what
/// `InputGate::may_act` reads to give a frame to the UI, so an unguarded write here closes
/// every gameplay input in the client. That is how it was first written, and
/// `player::tests`' dead-player test is what caught it.
fn close(window: &mut ResMut<'_, VendorWindow>, mode: &mut ResMut<'_, InputMode>) {
    if window.current.is_some() {
        window.current = None;
    }
    if **mode == InputMode::Vendor {
        **mode = InputMode::Playing;
    }
}

/// `ui/mod.rs`'s `set_mode` rule, which this module cannot reach across the boundary:
/// a mode is written only when it is a different mode.
fn set_mode(mode: &mut ResMut<'_, InputMode>, next: InputMode) {
    if **mode != next {
        **mode = next;
    }
}

/// Turns each press into one `TradeRequest`, written against the list on screen.
///
/// **The revision is what makes a one-message-old view safe to originate from.** A trade
/// against a list the server has replaced is refused rather than applied at prices the
/// player never saw, so this side never has to ask whether what it is showing is current —
/// it says which list it was looking at and lets the server answer.
///
/// **Nothing is checked here that the server checks**, and the one thing that *is* checked
/// is not a gameplay rule: a click naming an item that is not in the vector it claims to
/// come from is a defect in this build rather than a request, and sending it would spend a
/// tick asking a question whose answer is already known. Whether the purse covers it,
/// whether the pack has room and whether the player still holds what they are selling are
/// all the server's, and all three come back as an `ActionRefused` with a sentence in
/// `ui/status.rs`.
fn send_trade_intents(
    gate: InputGate<'_>,
    cadence: Res<InputCadence>,
    window: Res<VendorWindow>,
    outbound: Option<ResMut<Outbound>>,
    mut clicks: MessageReader<VendorTradeClick>,
) {
    // Read and dropped rather than left queued: a press that arrived on the frame the
    // window closed belongs to a stall that is gone, and holding it would send it at
    // whatever opened next.
    let presses: Vec<VendorTradeClick> = clicks.read().copied().collect();
    let Some(outbound) = outbound else {
        return;
    };
    if gate.mode() != InputMode::Vendor || gate.dead() {
        return;
    }
    let Some(state) = window.state() else {
        return;
    };
    let outbound = outbound.into_inner();
    for click in presses {
        let list = if click.buying {
            &state.sells
        } else {
            &state.buys
        };
        if !list.iter().any(|entry| entry.item_id == click.item_id) {
            continue;
        }
        outbound.send(encode_trade_request(&TradeRequest {
            entity_id: state.entity_id,
            item_id: click.item_id,
            count: click.count,
            buying: click.buying,
            revision: state.revision,
            client_tick: cadence.client_tick,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{
        ANY_TOKEN, LifeState, Outbound, PlayerVitals, SessionParams, VendorClosed, VendorEntry,
    };
    use crate::player::ViewMode;

    /// The two items the smith in [`state`] deals in, one each way.
    const PICKAXE: u16 = 6;
    const RAW_IRON: u16 = 12;

    const SMITH: u64 = (1 << 62) | 55;
    const COOK: u64 = (1 << 62) | 56;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.0; 3],
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

    fn state(entity_id: u64, revision: u32) -> VendorState {
        VendorState {
            entity_id,
            revision,
            sells: vec![VendorEntry {
                item_id: PICKAXE,
                price: 25,
            }],
            buys: vec![VendorEntry {
                item_id: RAW_IRON,
                price: 3,
            }],
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .init_resource::<InputCadence>()
            .add_plugins(VendorPlugin);
        app
    }

    fn push(app: &mut App, event: VendorEvent) {
        app.world_mut().resource_mut::<VendorInbox>().push(event);
        app.update();
    }

    /// A price list opens the window and takes the mode; the closure the server owes ends
    /// both, and a second list replaces the first wholesale.
    #[test]
    fn a_list_opens_the_stall_a_second_replaces_it_and_a_closure_ends_it() {
        let mut app = app();

        push(&mut app, VendorEvent::State(state(SMITH, 1)));
        assert_eq!(
            app.world().resource::<VendorWindow>().state(),
            Some(&state(SMITH, 1))
        );
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Vendor);

        push(&mut app, VendorEvent::State(state(COOK, 1)));
        assert_eq!(
            app.world().resource::<VendorWindow>().state(),
            Some(&state(COOK, 1))
        );

        // The first stall's closure, arriving after the second opened. It names a vendor
        // this window is no longer showing, so it must not take the second one down.
        push(
            &mut app,
            VendorEvent::Closed(VendorClosed { entity_id: SMITH }),
        );
        assert_eq!(
            app.world().resource::<VendorWindow>().state(),
            Some(&state(COOK, 1)),
            "a closure for the stall that was replaced took down the one that replaced it"
        );

        push(
            &mut app,
            VendorEvent::Closed(VendorClosed { entity_id: COOK }),
        );
        assert!(app.world().resource::<VendorWindow>().state().is_none());
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
    }

    /// **The same revision re-opens the window**, which is what re-addressing an open stall
    /// produces on the wire. A staleness guard copied from `loot.rs` would swallow it, and
    /// the interact key would do nothing at a stall closed with `Escape`.
    #[test]
    fn the_revision_the_server_already_sent_opens_the_window_again() {
        let mut app = app();
        push(&mut app, VendorEvent::State(state(SMITH, 1)));

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert!(
            app.world().resource::<VendorWindow>().state().is_none(),
            "escape left the mode and the window stayed up"
        );

        push(&mut app, VendorEvent::State(state(SMITH, 1)));
        assert_eq!(
            app.world().resource::<VendorWindow>().state(),
            Some(&state(SMITH, 1))
        );
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Vendor);
    }

    /// **Death takes the stall and leaves every other screen alone**, on every one of the
    /// frames a dead player is dead. The pause menu is deliberately not a screen death
    /// closes, and a mode handed back unconditionally would take a corpse out of the menu
    /// it is quitting from once a frame.
    #[test]
    fn death_does_not_take_a_dead_player_out_of_the_pause_menu() {
        let mut app = app();
        push(&mut app, VendorEvent::State(state(SMITH, 1)));
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;

        app.insert_resource(SelfVitals::from_server(PlayerVitals {
            health: 0,
            life_state: LifeState::Dead,
            ..PlayerVitals::unharmed()
        }));
        for _ in 0..3 {
            app.update();
            assert_eq!(*app.world().resource::<InputMode>(), InputMode::Menu);
        }
        assert!(
            app.world().resource::<VendorWindow>().state().is_none(),
            "the stall survived the player dying in front of it"
        );
    }

    /// **One press, one request, written against the list on screen** — and the count is
    /// the modifier the UI resolved rather than anything decided here. Neither request
    /// names a price or a total, because the contract has no field to put one in.
    #[test]
    fn a_press_sends_one_request_from_the_revision_on_screen() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(8);
        app.insert_resource(outbound);
        push(&mut app, VendorEvent::State(state(SMITH, 4)));
        assert!(frames.try_iter().collect::<Vec<_>>().is_empty());

        app.world_mut().write_message(VendorTradeClick {
            item_id: PICKAXE,
            buying: true,
            count: 1,
        });
        app.world_mut().write_message(VendorTradeClick {
            item_id: RAW_IRON,
            buying: false,
            count: SHIFT_COUNT,
        });
        app.update();

        assert_eq!(
            frames.try_iter().collect::<Vec<_>>(),
            vec![
                encode_trade_request(&TradeRequest {
                    entity_id: SMITH,
                    item_id: PICKAXE,
                    count: 1,
                    buying: true,
                    revision: 4,
                    client_tick: 0,
                }),
                encode_trade_request(&TradeRequest {
                    entity_id: SMITH,
                    item_id: RAW_IRON,
                    count: SHIFT_COUNT,
                    buying: false,
                    revision: 4,
                    client_tick: 0,
                }),
            ]
        );
    }

    /// **A press this build could not have drawn sends nothing**, in either direction.
    ///
    /// Not a gameplay rule and not a second opinion about the trade: the vendor's own
    /// answer to all three of these is `VendorDoesNotWant`, and the server still gives it
    /// if one ever leaves. What it prevents is a defect in this build spending a tick
    /// asking a question whose answer is already on screen — the pickaxe is in `sells` and
    /// asking to sell one is asking against the wrong vector.
    #[test]
    fn a_press_naming_a_row_the_stall_does_not_show_sends_nothing() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(8);
        app.insert_resource(outbound);
        push(&mut app, VendorEvent::State(state(SMITH, 1)));

        for click in [
            VendorTradeClick {
                item_id: PICKAXE,
                buying: false,
                count: 1,
            },
            VendorTradeClick {
                item_id: RAW_IRON,
                buying: true,
                count: 1,
            },
            VendorTradeClick {
                item_id: 999,
                buying: true,
                count: 1,
            },
        ] {
            app.world_mut().write_message(click);
        }
        app.update();
        assert!(frames.try_iter().collect::<Vec<_>>().is_empty());
    }

    /// A press that outlived the window sends nothing, and is not held for the next stall.
    #[test]
    fn a_press_that_outlived_the_window_is_dropped_rather_than_queued() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(8);
        app.insert_resource(outbound);
        push(&mut app, VendorEvent::State(state(SMITH, 1)));

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.world_mut().write_message(VendorTradeClick {
            item_id: PICKAXE,
            buying: true,
            count: 1,
        });
        app.update();
        assert!(frames.try_iter().collect::<Vec<_>>().is_empty());

        push(&mut app, VendorEvent::State(state(COOK, 1)));
        assert!(
            frames.try_iter().collect::<Vec<_>>().is_empty(),
            "a press meant for one stall was delivered to the next"
        );
    }

    /// Death closes the stall, and so does losing the session.
    #[test]
    fn death_and_a_lost_session_both_close_the_stall() {
        let mut app = app();
        push(&mut app, VendorEvent::State(state(SMITH, 1)));

        app.insert_resource(SelfVitals::from_server(PlayerVitals {
            health: 0,
            life_state: LifeState::Dead,
            ..PlayerVitals::unharmed()
        }));
        app.update();
        assert!(app.world().resource::<VendorWindow>().state().is_none());
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);

        app.insert_resource(SelfVitals::default());
        push(&mut app, VendorEvent::State(state(SMITH, 1)));
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Vendor);

        app.world_mut().remove_resource::<Session>();
        app.update();
        assert!(app.world().resource::<VendorWindow>().state().is_none());
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
    }
}
