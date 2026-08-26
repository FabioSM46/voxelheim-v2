//! Client-side presentation and intent for server-owned corpse containers.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use super::{
    ApplyInputMode, ApplySnapshots, InputCadence, InputGate, InputMode, SelfVitals, SnapshotBuffer,
};
use crate::net::{
    LootEvent, LootInbox, LootOpenRequest, LootState, LootTakeRequest, Outbound, Session,
    encode_loot_open_request, encode_loot_take_request,
};
use crate::settings::{Control, Settings};

use super::constants::MAX_REACH;

/// The newest complete corpse-container answer currently shown.
#[derive(Resource, Debug, Default)]
pub struct LootWindow {
    current: Option<LootState>,
    newest_revision: HashMap<u64, u32>,
    dismissed: HashSet<u64>,
}

impl LootWindow {
    pub fn state(&self) -> Option<&LootState> {
        self.current.as_ref()
    }

    fn dismiss_current(&mut self) {
        if let Some(state) = self.current.take() {
            self.dismissed.insert(state.corpse_id);
        }
    }

    #[cfg(test)]
    pub(crate) fn from_server(state: LootState) -> Self {
        Self {
            newest_revision: HashMap::from([(state.corpse_id, state.revision)]),
            current: Some(state),
            dismissed: HashSet::new(),
        }
    }
}

/// A click on one whole authoritative loot entry.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootTakeClick(pub u64);

pub(super) struct LootPlugin;

impl Plugin for LootPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LootWindow>()
            .init_resource::<LootInbox>()
            .add_message::<LootTakeClick>()
            .add_systems(
                Update,
                reconcile_loot
                    .after(crate::net::DrainNetwork)
                    .after(ApplySnapshots)
                    .before(ApplyInputMode),
            )
            .add_systems(
                Update,
                send_loot_intents
                    .after(ApplyInputMode)
                    .after(ApplySnapshots),
            );
    }
}

/// Applies server answers in order and closes any view the newest snapshot invalidated.
fn reconcile_loot(
    mut inbox: ResMut<LootInbox>,
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut window: ResMut<LootWindow>,
    mut mode: ResMut<InputMode>,
) {
    for event in inbox.take() {
        match event {
            LootEvent::State(state) => {
                let stale = window
                    .newest_revision
                    .get(&state.corpse_id)
                    .is_some_and(|newest| state.revision <= *newest);
                if stale
                    || window.dismissed.contains(&state.corpse_id)
                    || !buffer.corpse_is_accessible(state.corpse_id)
                {
                    continue;
                }
                window
                    .newest_revision
                    .insert(state.corpse_id, state.revision);
                window.current = Some(state);
                *mode = InputMode::Loot;
            }
            LootEvent::Closed(closed) => {
                window.dismissed.insert(closed.corpse_id);
                if window
                    .current
                    .as_ref()
                    .is_some_and(|state| state.corpse_id == closed.corpse_id)
                {
                    window.current = None;
                    if *mode == InputMode::Loot {
                        *mode = InputMode::Playing;
                    }
                }
            }
        }
    }

    if session.is_none() {
        *window = LootWindow::default();
        if *mode == InputMode::Loot {
            *mode = InputMode::Playing;
        }
        return;
    }

    let invalid = vitals.dead()
        || window
            .current
            .as_ref()
            .is_some_and(|state| !buffer.corpse_is_accessible(state.corpse_id));
    if invalid {
        window.dismiss_current();
        if *mode == InputMode::Loot {
            *mode = InputMode::Playing;
        }
    }
}

/// Originates open/take requests; neither path edits the shown container or inventory.
#[derive(bevy::ecs::system::SystemParam)]
struct LootIntent<'w> {
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    settings: Option<Res<'w, Settings>>,
    gate: InputGate<'w>,
    session: Option<Res<'w, Session>>,
    buffer: Res<'w, SnapshotBuffer>,
    cadence: Res<'w, InputCadence>,
    outbound: Option<ResMut<'w, Outbound>>,
}

fn send_loot_intents(
    intent: LootIntent<'_>,
    mut clicks: MessageReader<LootTakeClick>,
    mut window: ResMut<LootWindow>,
) {
    let LootIntent {
        keys,
        settings,
        gate,
        session,
        buffer,
        cadence,
        mut outbound,
    } = intent;
    if window.current.is_some() && gate.mode() != InputMode::Loot {
        window.dismiss_current();
    }

    if gate.mode() == InputMode::Loot && !gate.dead() {
        let Some(outbound) = outbound.as_deref_mut() else {
            return;
        };
        for click in clicks.read() {
            let Some(state) = window.current.as_ref() else {
                continue;
            };
            if !state.entries.iter().any(|entry| entry.entry_id == click.0) {
                continue;
            }
            outbound.send(encode_loot_take_request(&LootTakeRequest {
                corpse_id: state.corpse_id,
                entry_id: click.0,
                revision: state.revision,
                client_tick: cadence.client_tick,
            }));
        }
        return;
    }

    let bindings = settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());
    if !gate.may_act()
        || !keys.is_some_and(|keys| keys.just_pressed(bindings.key(Control::Interact)))
    {
        return;
    }
    let (Some(session), Some(outbound)) = (session, outbound.as_deref_mut()) else {
        return;
    };
    let Some(corpse_id) = buffer.nearest_accessible_corpse(session.0.entity_id, MAX_REACH) else {
        return;
    };
    window.dismissed.remove(&corpse_id);
    window.newest_revision.remove(&corpse_id);
    outbound.send(encode_loot_open_request(&LootOpenRequest {
        corpse_id,
        client_tick: cadence.client_tick,
    }));
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::net::{
        ANY_TOKEN, EntityState, LifeState, LootClosed, LootEntry, MobAction, MobKind, MobState,
        PlayerVitals, SessionParams, Snapshot,
    };
    use crate::player::ViewMode;

    const PLAYER: u64 = 7;
    const CORPSE: u64 = 40;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: PLAYER,
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

    fn snapshot() -> Snapshot {
        Snapshot {
            server_tick: 1,
            entities: vec![EntityState {
                entity_id: PLAYER,
                pos: [0.0, 64.0, 0.0],
                vel: [0.0; 3],
                yaw: 0.0,
            }],
            mobs: vec![MobState {
                entity_id: CORPSE,
                kind: MobKind::Draugr,
                pos: [2.0, 64.0, 0.0],
                vel: [0.0; 3],
                yaw: 0.0,
                health: 0,
                max_health: 60,
                action: MobAction::Corpse,
                target_entity_id: 0,
            }],
            accessible_loot_corpses: vec![CORPSE],
            ..Default::default()
        }
    }

    fn state(revision: u32, entry_id: u64) -> LootState {
        LootState {
            corpse_id: CORPSE,
            revision,
            entries: vec![LootEntry {
                entry_id,
                item_id: 1,
                count: 2,
                durability: 0,
                max_durability: 0,
            }],
        }
    }

    fn app() -> App {
        let mut buffer = SnapshotBuffer::default();
        assert!(buffer.accept(snapshot(), Instant::now()));
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(buffer)
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .init_resource::<InputCadence>()
            .add_plugins(LootPlugin);
        app
    }

    #[test]
    fn newer_states_replace_wholesale_and_close_prevents_a_stale_reopen() {
        let mut app = app();
        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(2, 20)));
        app.update();
        assert_eq!(
            app.world().resource::<LootWindow>().state(),
            Some(&state(2, 20))
        );
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Loot);

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(1, 10)));
        app.update();
        assert_eq!(
            app.world().resource::<LootWindow>().state(),
            Some(&state(2, 20))
        );

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(2, 99)));
        app.update();
        assert_eq!(
            app.world().resource::<LootWindow>().state(),
            Some(&state(2, 20)),
            "an equal revision cannot restore a consumed entry"
        );

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(3, 30)));
        app.update();
        assert_eq!(
            app.world().resource::<LootWindow>().state(),
            Some(&state(3, 30))
        );

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::Closed(LootClosed { corpse_id: CORPSE }));
        app.update();
        assert!(app.world().resource::<LootWindow>().state().is_none());
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(3, 30)));
        app.update();
        assert!(app.world().resource::<LootWindow>().state().is_none());
    }

    #[test]
    fn interact_and_take_send_only_intent_and_keep_the_authoritative_entries() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(4);
        app.insert_resource(outbound)
            .insert_resource(ButtonInput::<KeyCode>::default());
        app.update();

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyF);
        app.update();
        assert_eq!(
            frames.try_recv().unwrap(),
            encode_loot_open_request(&LootOpenRequest {
                corpse_id: CORPSE,
                client_tick: 0,
            })
        );

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(4, 44)));
        app.update();
        app.world_mut().write_message(LootTakeClick(44));
        app.update();
        assert_eq!(
            frames.try_recv().unwrap(),
            encode_loot_take_request(&LootTakeRequest {
                corpse_id: CORPSE,
                entry_id: 44,
                revision: 4,
                client_tick: 0,
            })
        );
        assert_eq!(
            app.world().resource::<LootWindow>().state(),
            Some(&state(4, 44))
        );

        let dead = PlayerVitals {
            health: 0,
            life_state: LifeState::Dead,
            ..PlayerVitals::unharmed()
        };
        app.insert_resource(SelfVitals::from_server(dead));
        app.update();
        assert!(app.world().resource::<LootWindow>().state().is_none());
    }
}
