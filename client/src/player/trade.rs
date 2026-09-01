//! The latest complete player-trade view the server sent.
//!
//! This module deliberately owns no buttons and originates no request. It is the state
//! half of the trade boundary: ordered network events arrive, a complete state replaces
//! the previous one wholesale, and a matching close removes it. The presentation layer
//! can therefore draw one authoritative value without keeping a second copy.

use bevy::prelude::*;

use super::ApplySnapshots;
use crate::net::{PlayerTradeEvent, PlayerTradeInbox, PlayerTradeState, Session};

/// The one player trade this session can currently see.
#[derive(Resource, Debug, Default)]
pub struct PlayerTradeWindow {
    current: Option<PlayerTradeState>,
}

impl PlayerTradeWindow {
    #[cfg(test)]
    fn state(&self) -> Option<&PlayerTradeState> {
        self.current.as_ref()
    }
}

pub(super) struct PlayerTradePlugin;

impl Plugin for PlayerTradePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerTradeWindow>()
            .init_resource::<PlayerTradeInbox>()
            .add_systems(
                Update,
                reconcile_player_trade
                    .after(crate::net::DrainNetwork)
                    .after(ApplySnapshots),
            );
    }
}

/// Applies every server answer in wire order and keeps no state past its session.
fn reconcile_player_trade(
    mut inbox: ResMut<PlayerTradeInbox>,
    session: Option<Res<Session>>,
    mut window: ResMut<PlayerTradeWindow>,
) {
    for event in inbox.take() {
        match event {
            PlayerTradeEvent::State(state) => window.current = Some(state),
            PlayerTradeEvent::Closed(closed) => {
                if window
                    .current
                    .as_ref()
                    .is_some_and(|state| state.partner_entity_id == closed.partner_entity_id)
                {
                    window.current = None;
                }
            }
        }
    }

    if session.is_none() && window.current.is_some() {
        window.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{
        ANY_TOKEN, PlayerTradeCloseReason, PlayerTradeClosed, PlayerTradeEvent, SessionParams,
        WorldClock,
    };

    fn state(partner_entity_id: u64, revision: u32) -> PlayerTradeState {
        PlayerTradeState {
            partner_entity_id,
            partner_name: format!("Player {partner_entity_id}"),
            revision,
            my_offer: Vec::new(),
            their_offer: Vec::new(),
            my_silver: 0,
            their_silver: 0,
            my_confirmed: false,
            their_confirmed: false,
        }
    }

    fn session() -> Session {
        Session(SessionParams {
            entity_id: 7,
            spawn: [0.5, 64.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: ANY_TOKEN,
            clock: WorldClock::default(),
        })
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .add_plugins(PlayerTradePlugin);
        app
    }

    fn push(app: &mut App, event: PlayerTradeEvent) {
        app.world_mut()
            .resource_mut::<PlayerTradeInbox>()
            .push(event);
        app.update();
    }

    #[test]
    fn every_state_replaces_the_previous_view_wholesale() {
        let mut app = app();
        push(&mut app, PlayerTradeEvent::State(state(11, 1)));
        push(&mut app, PlayerTradeEvent::State(state(13, 2)));

        assert_eq!(
            app.world().resource::<PlayerTradeWindow>().state(),
            Some(&state(13, 2))
        );
    }

    #[test]
    fn only_the_current_partners_close_ends_the_view() {
        let mut app = app();
        push(&mut app, PlayerTradeEvent::State(state(11, 1)));
        push(
            &mut app,
            PlayerTradeEvent::Closed(PlayerTradeClosed {
                partner_entity_id: 13,
                reason: PlayerTradeCloseReason::Cancelled,
            }),
        );
        assert!(
            app.world()
                .resource::<PlayerTradeWindow>()
                .state()
                .is_some()
        );

        push(
            &mut app,
            PlayerTradeEvent::Closed(PlayerTradeClosed {
                partner_entity_id: 11,
                reason: PlayerTradeCloseReason::Completed,
            }),
        );
        assert!(
            app.world()
                .resource::<PlayerTradeWindow>()
                .state()
                .is_none()
        );
    }

    #[test]
    fn state_and_close_are_applied_in_wire_order() {
        let closed = PlayerTradeEvent::Closed(PlayerTradeClosed {
            partner_entity_id: 11,
            reason: PlayerTradeCloseReason::Cancelled,
        });

        let mut close_then_state = app();
        close_then_state
            .world_mut()
            .resource_mut::<PlayerTradeInbox>()
            .push(closed.clone());
        close_then_state
            .world_mut()
            .resource_mut::<PlayerTradeInbox>()
            .push(PlayerTradeEvent::State(state(11, 2)));
        close_then_state.update();
        assert_eq!(
            close_then_state
                .world()
                .resource::<PlayerTradeWindow>()
                .state(),
            Some(&state(11, 2))
        );

        let mut state_then_close = app();
        state_then_close
            .world_mut()
            .resource_mut::<PlayerTradeInbox>()
            .push(PlayerTradeEvent::State(state(11, 2)));
        state_then_close
            .world_mut()
            .resource_mut::<PlayerTradeInbox>()
            .push(closed);
        state_then_close.update();
        assert!(
            state_then_close
                .world()
                .resource::<PlayerTradeWindow>()
                .state()
                .is_none()
        );
    }

    #[test]
    fn losing_the_session_clears_the_authoritative_view() {
        let mut app = app();
        push(&mut app, PlayerTradeEvent::State(state(11, 1)));
        app.world_mut().remove_resource::<Session>();
        app.update();

        assert!(
            app.world()
                .resource::<PlayerTradeWindow>()
                .state()
                .is_none()
        );
    }
}
