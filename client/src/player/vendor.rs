//! Client-side presentation of one server-owned stall.
//!
//! **The mirror of `loot.rs`, and deliberately smaller than it.** A corpse is a container
//! whose contents this client watches empty; a stall is a price list, and a price list has
//! nothing consumable in it. What is left after that difference is the session itself: the
//! server decides which stall a player has open, says so with a `VendorState`, and ends it
//! with a `VendorClosed`. This module holds the newest list it was sent and the mode that
//! goes with it, and decides nothing else.

use bevy::prelude::*;

use super::{ApplyInputMode, ApplySnapshots, InputMode, SelfVitals};
use crate::net::{Session, VendorEvent, VendorInbox, VendorState};

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

pub(super) struct VendorPlugin;

impl Plugin for VendorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VendorWindow>()
            .init_resource::<VendorInbox>()
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
                dismiss_the_stall_the_player_left.after(ApplyInputMode),
            );
    }
}

/// Applies the server's answers in order, and closes a window the newest one invalidated.
///
/// **There is no staleness guard here and `loot.rs` has one, which is a difference worth
/// stating rather than leaving to look like an omission.** Loot refuses a state whose
/// revision it has already seen because an equal revision arriving late would restore an
/// entry the player has already taken. A stall has no such entry: `VendorState` replaces
/// the client's view of that vendor wholesale by contract, and the server deliberately
/// re-sends *the revision it already has* when a player addresses the stall they already
/// have open (`openVendorLocked` in `server/internal/game/vendor.go`). A guard written
/// from loot's shape would swallow exactly that re-send, and pressing the interact key at
/// a stall the player had closed with `Escape` would do nothing at all.
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
                // Whatever was open is replaced, without a closure being needed for it:
                // the server closes the first stall when a second opens and queues its
                // `VendorClosed`, but the two frames may arrive in either order and only
                // one window exists to show either in.
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
    // session. Neither is this client deciding anything — the server refuses every trade
    // from a corpse through the same act gate that refuses mining — it is a window that
    // must not sit over a death overlay showing prices nobody can pay.
    if session.is_none() || vitals.dead() {
        close(&mut window, &mut mode);
    }
}

/// Takes the window down when the mode has left it.
///
/// The one gesture this client answers on its own: `Escape` changes the mode, and the list
/// it was showing goes with it. Nothing is sent — there is no message for "I have stopped
/// looking", by contract — so the server keeps the session until the player walks away or
/// addresses somebody else, and re-addressing this stall re-sends the list it already had.
fn dismiss_the_stall_the_player_left(mode: Res<InputMode>, mut window: ResMut<VendorWindow>) {
    if window.current.is_some() && *mode != InputMode::Vendor {
        window.current = None;
    }
}

/// Takes the window down and hands the mode back, writing to neither unless it moves.
///
/// **Both guards are load-bearing and neither is an optimisation.** Dereferencing a
/// `ResMut` marks its resource changed whether or not the value moved, and `InputMode`'s
/// change flag is exactly what `InputGate::may_act` reads to give the frame a mode changed
/// on to the UI. This runs every frame, so an unguarded write here would mark the mode
/// changed twenty times a second and close every gameplay input in the client — which is
/// how it was first written, and what `player::tests`' dead-player test caught.
fn close(window: &mut ResMut<'_, VendorWindow>, mode: &mut ResMut<'_, InputMode>) {
    if window.current.is_some() {
        window.current = None;
    }
    set_mode(mode, InputMode::Playing);
}

/// `ui/mod.rs`'s `set_mode` rule, which this module cannot reach across the boundary:
/// a mode is written only when it is a different mode.
fn set_mode(mode: &mut ResMut<'_, InputMode>, next: InputMode) {
    if **mode != next {
        **mode = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{
        ANY_TOKEN, LifeState, PlayerVitals, SessionParams, VendorClosed, VendorEntry,
    };

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
                item_id: 6,
                price: 25,
            }],
            buys: vec![VendorEntry {
                item_id: 12,
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
            .add_plugins(VendorPlugin);
        app
    }

    fn push(app: &mut App, event: VendorEvent) {
        app.world_mut().resource_mut::<VendorInbox>().push(event);
        app.update();
    }

    /// A price list opens the window and takes the mode; the closure the server owes ends
    /// both. The second list replaces the first wholesale rather than merging into it.
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

    /// **The same revision re-opens the window**, which is what re-addressing an open
    /// stall produces on the wire: the server keeps the session and re-sends the list it
    /// already had. A staleness guard copied from `loot.rs` would swallow it, and the
    /// interact key would do nothing at a stall the player had closed with `Escape`.
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
