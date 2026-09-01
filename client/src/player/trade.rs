use bevy::prelude::*;

use super::{ApplyInputMode, ApplySnapshots, InputCadence, InputMode, SelfVitals};
use crate::net::{
    Outbound, PLAYER_TRADE_SLOTS, PlayerTradeAction, PlayerTradeCloseReason, PlayerTradeEvent,
    PlayerTradeInbox, PlayerTradeRequest, PlayerTradeState, Session, encode_player_trade_request,
};

#[derive(Resource, Debug, Default)]
pub struct PlayerTradeWindow {
    current: Option<PlayerTradeState>,
    locally_cancelled_partner: Option<u64>,
}

impl PlayerTradeWindow {
    pub fn state(&self) -> Option<&PlayerTradeState> {
        self.current.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn from_server(state: PlayerTradeState) -> Self {
        Self {
            current: Some(state),
            locally_cancelled_partner: None,
        }
    }
}

#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerTradeClick {
    OfferPackSlot(u8),
    ClearOfferSlot(u8),
    SetSilver(u32),
    Confirm,
    Cancel,
}

#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct PlayerTradeEnded {
    pub partner_name: String,
    pub reason: PlayerTradeCloseReason,
}

pub(super) struct PlayerTradePlugin;

impl Plugin for PlayerTradePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerTradeWindow>()
            .init_resource::<PlayerTradeInbox>()
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<InputCadence>()
            .add_message::<PlayerTradeClick>()
            .add_message::<PlayerTradeEnded>()
            .add_systems(
                Update,
                reconcile_player_trade
                    .after(crate::net::DrainNetwork)
                    .after(ApplySnapshots)
                    .before(ApplyInputMode),
            )
            .add_systems(Update, send_player_trade_intents.after(ApplyInputMode));
    }
}

fn reconcile_player_trade(
    mut inbox: ResMut<PlayerTradeInbox>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut window: ResMut<PlayerTradeWindow>,
    mut mode: ResMut<InputMode>,
    mut ended: MessageWriter<PlayerTradeEnded>,
) {
    for event in inbox.take() {
        match event {
            PlayerTradeEvent::State(state) => {
                if window.locally_cancelled_partner == Some(state.partner_entity_id) {
                    continue;
                }
                window.locally_cancelled_partner = None;
                window.current = Some(state);
                if matches!(*mode, InputMode::Playing | InputMode::Trade) {
                    set_mode(&mut mode, InputMode::Trade);
                }
            }
            PlayerTradeEvent::Closed(closed) => {
                if window.locally_cancelled_partner == Some(closed.partner_entity_id) {
                    window.locally_cancelled_partner = None;
                    continue;
                }
                if window
                    .current
                    .as_ref()
                    .is_some_and(|state| state.partner_entity_id == closed.partner_entity_id)
                {
                    let state = window.current.take().expect("the matching state exists");
                    ended.write(PlayerTradeEnded {
                        partner_name: state.partner_name,
                        reason: closed.reason,
                    });
                    if *mode == InputMode::Trade {
                        *mode = InputMode::Playing;
                    }
                }
            }
        }
    }

    if session.is_none() || vitals.dead() {
        close_locally(&mut window, &mut mode);
    }
}

fn send_player_trade_intents(
    cadence: Res<InputCadence>,
    vitals: Res<SelfVitals>,
    mut mode: ResMut<InputMode>,
    mut window: ResMut<PlayerTradeWindow>,
    outbound: Option<ResMut<Outbound>>,
    mut clicks: MessageReader<PlayerTradeClick>,
) {
    let clicks: Vec<PlayerTradeClick> = clicks.read().copied().collect();
    if window.current.is_none() {
        return;
    }

    if vitals.dead() {
        close_locally(&mut window, &mut mode);
        return;
    }

    let Some(mut outbound) = outbound else {
        return;
    };
    for click in clicks {
        let Some(state) = window.current.as_ref() else {
            break;
        };
        if *mode != InputMode::Trade && click != PlayerTradeClick::Cancel {
            continue;
        }
        let request = match click {
            PlayerTradeClick::Cancel => Some((PlayerTradeAction::Cancel, 0, 0, 0)),
            PlayerTradeClick::Confirm if !state.my_confirmed => {
                Some((PlayerTradeAction::Confirm, 0, 0, 0))
            }
            PlayerTradeClick::SetSilver(silver) if !state.my_confirmed => {
                Some((PlayerTradeAction::SetSilver, 0, 0, silver))
            }
            PlayerTradeClick::ClearOfferSlot(trade_slot)
                if !state.my_confirmed
                    && state
                        .my_offer
                        .iter()
                        .any(|slot| slot.trade_slot == trade_slot) =>
            {
                Some((PlayerTradeAction::ClearItem, trade_slot, 0, 0))
            }
            PlayerTradeClick::OfferPackSlot(pack_slot) if !state.my_confirmed => {
                lowest_empty_slot(state)
                    .map(|trade_slot| (PlayerTradeAction::SetItem, trade_slot, pack_slot, 0))
            }
            PlayerTradeClick::Confirm
            | PlayerTradeClick::SetSilver(_)
            | PlayerTradeClick::ClearOfferSlot(_)
            | PlayerTradeClick::OfferPackSlot(_) => None,
        };
        let Some((action, trade_slot, pack_slot, silver)) = request else {
            continue;
        };
        send(
            &mut outbound,
            state,
            action,
            trade_slot,
            pack_slot,
            silver,
            cadence.client_tick,
        );
        if action == PlayerTradeAction::Cancel {
            let partner_entity_id = state.partner_entity_id;
            window.current = None;
            window.locally_cancelled_partner = Some(partner_entity_id);
            set_mode(&mut mode, InputMode::Playing);
        }
    }
}

fn lowest_empty_slot(state: &PlayerTradeState) -> Option<u8> {
    (0..PLAYER_TRADE_SLOTS as u8).find(|candidate| {
        !state
            .my_offer
            .iter()
            .any(|slot| slot.trade_slot == *candidate)
    })
}

fn send(
    outbound: &mut Outbound,
    state: &PlayerTradeState,
    action: PlayerTradeAction,
    trade_slot: u8,
    pack_slot: u8,
    silver: u32,
    client_tick: u32,
) {
    outbound.send(encode_player_trade_request(&PlayerTradeRequest {
        action,
        target_entity_id: state.partner_entity_id,
        trade_slot,
        pack_slot,
        silver,
        revision: state.revision,
        client_tick,
    }));
}

fn close_locally(window: &mut ResMut<'_, PlayerTradeWindow>, mode: &mut ResMut<'_, InputMode>) {
    if window.current.is_some() {
        window.current = None;
    }
    window.locally_cancelled_partner = None;
    if **mode == InputMode::Trade {
        **mode = InputMode::Playing;
    }
}

fn set_mode(mode: &mut ResMut<'_, InputMode>, next: InputMode) {
    if **mode != next {
        **mode = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{
        ANY_TOKEN, PlayerTradeCloseReason, PlayerTradeClosed, PlayerTradeEvent, PlayerTradeSlot,
        SessionParams, WorldClock,
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
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Trade);
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
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<PlayerTradeEnded>>()
                .drain()
                .collect::<Vec<_>>(),
            vec![PlayerTradeEnded {
                partner_name: "Player 11".to_owned(),
                reason: PlayerTradeCloseReason::Completed,
            }]
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

    #[test]
    fn every_window_press_carries_the_current_partner_revision_and_tick() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(8);
        app.insert_resource(outbound);
        let mut current = state(11, 4);
        current.my_offer.push(PlayerTradeSlot {
            trade_slot: 1,
            pack_slot: 9,
            item_id: 3,
            count: 2,
            durability: 0,
            max_durability: 0,
        });
        push(&mut app, PlayerTradeEvent::State(current));
        app.world_mut().resource_mut::<InputCadence>().client_tick = 77;

        for click in [
            PlayerTradeClick::OfferPackSlot(10),
            PlayerTradeClick::ClearOfferSlot(1),
            PlayerTradeClick::SetSilver(23),
            PlayerTradeClick::Confirm,
        ] {
            app.world_mut().write_message(click);
        }
        app.update();

        let request = |action, trade_slot, pack_slot, silver| {
            encode_player_trade_request(&PlayerTradeRequest {
                action,
                target_entity_id: 11,
                trade_slot,
                pack_slot,
                silver,
                revision: 4,
                client_tick: 77,
            })
        };
        assert_eq!(
            frames.try_iter().collect::<Vec<_>>(),
            vec![
                request(PlayerTradeAction::SetItem, 0, 10, 0),
                request(PlayerTradeAction::ClearItem, 1, 0, 0),
                request(PlayerTradeAction::SetSilver, 0, 0, 23),
                request(PlayerTradeAction::Confirm, 0, 0, 0),
            ]
        );
        assert_eq!(
            app.world().resource::<PlayerTradeWindow>().state(),
            Some(&state_with_offer())
        );
    }

    fn state_with_offer() -> PlayerTradeState {
        let mut state = state(11, 4);
        state.my_offer.push(PlayerTradeSlot {
            trade_slot: 1,
            pack_slot: 9,
            item_id: 3,
            count: 2,
            durability: 0,
            max_durability: 0,
        });
        state
    }

    #[test]
    fn explicit_cancel_sends_once_and_ignores_late_state_but_death_sends_nothing() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(4);
        app.insert_resource(outbound);
        push(&mut app, PlayerTradeEvent::State(state(11, 3)));
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.world_mut().write_message(PlayerTradeClick::Cancel);
        app.update();
        assert_eq!(
            frames.try_iter().collect::<Vec<_>>(),
            vec![encode_player_trade_request(&PlayerTradeRequest {
                action: PlayerTradeAction::Cancel,
                target_entity_id: 11,
                trade_slot: 0,
                pack_slot: 0,
                silver: 0,
                revision: 3,
                client_tick: 0,
            })]
        );

        push(&mut app, PlayerTradeEvent::State(state(11, 4)));
        assert!(
            app.world()
                .resource::<PlayerTradeWindow>()
                .state()
                .is_none(),
            "late state stays dismissed"
        );
        push(
            &mut app,
            PlayerTradeEvent::Closed(PlayerTradeClosed {
                partner_entity_id: 11,
                reason: PlayerTradeCloseReason::Cancelled,
            }),
        );
        push(&mut app, PlayerTradeEvent::State(state(11, 5)));
        app.insert_resource(SelfVitals::from_server(crate::net::PlayerVitals {
            health: 0,
            life_state: crate::net::LifeState::Dead,
            ..crate::net::PlayerVitals::unharmed()
        }));
        app.update();
        assert!(frames.try_iter().collect::<Vec<_>>().is_empty());
        assert!(
            app.world()
                .resource::<PlayerTradeWindow>()
                .state()
                .is_none()
        );
    }
}
