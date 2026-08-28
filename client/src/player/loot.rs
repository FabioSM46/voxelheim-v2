//! Client-side presentation and intent for server-owned corpse containers.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use super::{
    ApplyInputMode, ApplySnapshots, InputCadence, InputGate, InputMode, SelfVitals, SnapshotBuffer,
};
use crate::net::{
    LootEvent, LootInbox, LootOpenRequest, LootState, LootTakeAllRequest, LootTakeRequest,
    Outbound, Session, encode_loot_open_request, encode_loot_take_all_request,
    encode_loot_take_request,
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

    // Read once, above the branch, because interact now means two things and the key is
    // edge-triggered: `just_pressed` is cleared per frame rather than per reader, so a
    // second read in the other arm would be a second press to whichever arm ran first.
    let bindings = settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());
    let interact = keys.is_some_and(|keys| keys.just_pressed(bindings.key(Control::Interact)));

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
        // The same key that opened the corpse empties it. One request from the revision
        // currently on screen, and no opinion about what will fit: a pack that cannot hold
        // everything is answered with the remainder and a refusal, and this side finds out
        // the way it finds out about every other outcome.
        if interact && let Some(state) = window.current.as_ref() {
            outbound.send(encode_loot_take_all_request(&LootTakeAllRequest {
                corpse_id: state.corpse_id,
                revision: state.revision,
                client_tick: cadence.client_tick,
            }));
        }
        return;
    }

    if !gate.may_act() || !interact {
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
    use std::sync::mpsc::Receiver;
    use std::time::Instant;

    use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
    use bevy::input::{ButtonState, InputPlugin};

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

    /// One keyboard event in the shape winit delivers it.
    ///
    /// Written as a message rather than poked into [`ButtonInput`] because that resource is
    /// `keyboard_input_system`'s to maintain: it clears `just_pressed` at the top of every
    /// frame, so a press set directly reaches `Update` already forgotten.
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

    /// The loot module with the real keyboard pipeline behind it and somewhere to send.
    fn held_key_app() -> (App, Receiver<Vec<u8>>) {
        let mut app = app();
        app.add_plugins(InputPlugin);
        let (outbound, frames) = Outbound::to_a_test(8);
        app.insert_resource(outbound);
        app.update();
        (app, frames)
    }

    /// Runs one frame after delivering the events winit would have delivered for it, and
    /// reports the open requests it originated.
    fn keyboard_frame(
        app: &mut App,
        frames: &Receiver<Vec<u8>>,
        events: impl IntoIterator<Item = KeyboardInput>,
    ) -> Vec<Vec<u8>> {
        for event in events {
            app.world_mut().write_message(event);
        }
        app.update();
        frames.try_iter().collect()
    }

    /// **The same property as `ui::inventory`'s
    /// `holding_the_consume_key_reports_one_press_and_a_later_press_reports_again`, on the
    /// other request this client originates from a bound key** — so "a held key is one
    /// press" is pinned as a rule rather than as a one-off for consume.
    ///
    /// It answers the same review finding on PR #403: `keyboard_input_system` does not
    /// filter `KeyboardInput { repeat: true }`, and what keeps a repeat from re-arming
    /// `just_pressed` is `bevy_input`'s `press()`, a **dependency's** guarantee that no
    /// test in this tree used to touch. Holding interact next to a corpse must ask to open
    /// it once, not once per repeat frame.
    #[test]
    fn holding_interact_asks_to_open_a_corpse_once_and_a_later_press_asks_again() {
        const KEY: KeyCode = KeyCode::KeyF;
        let open = encode_loot_open_request(&LootOpenRequest {
            corpse_id: CORPSE,
            client_tick: 0,
        });
        let (mut app, frames) = held_key_app();
        let mut sent = Vec::new();

        sent.extend(keyboard_frame(
            &mut app,
            &frames,
            [key_event(KEY, ButtonState::Pressed, false)],
        ));
        // The repeats winit sends while the key is down, then a frame with no event at all,
        // which is the same held key on a machine with key repeat switched off.
        for _ in 0..3 {
            sent.extend(keyboard_frame(
                &mut app,
                &frames,
                [key_event(KEY, ButtonState::Pressed, true)],
            ));
            assert!(
                app.world().resource::<ButtonInput<KeyCode>>().pressed(KEY),
                "a repeat is not a release"
            );
        }
        sent.extend(keyboard_frame(&mut app, &frames, []));
        assert!(
            app.world().resource::<ButtonInput<KeyCode>>().pressed(KEY),
            "a silent frame is not a release either"
        );
        assert_eq!(sent, vec![open.clone()], "a held key is one press");

        // Release and press again, so this cannot pass by the key having stopped working.
        sent.extend(keyboard_frame(
            &mut app,
            &frames,
            [key_event(KEY, ButtonState::Released, false)],
        ));
        assert!(
            !app.world().resource::<ButtonInput<KeyCode>>().pressed(KEY),
            "the key was let go"
        );
        sent.extend(keyboard_frame(
            &mut app,
            &frames,
            [key_event(KEY, ButtonState::Pressed, false)],
        ));
        assert_eq!(
            sent,
            vec![open.clone(), open],
            "a press after a release is a second press"
        );
    }

    /// **Interact opens the corpse on the tick the creature died, while the body is still
    /// falling over.**
    ///
    /// The rhythm #441 is about, pinned from the side that decides it. The snapshot pair
    /// here is exactly what the server now produces — the creature alive and hunting in one
    /// tick, a corpse in `accessible_loot_corpses` in the very next — and this frame is the
    /// first one that has ever seen the corpse, so on the client the body has not begun to
    /// tip yet. It still opens, because there is nothing on this path that could ask: the
    /// intent is originated from `SnapshotBuffer::nearest_accessible_corpse`, which reads
    /// the newest snapshot's mobs and its accessible-corpse vector and nothing else. No
    /// `Mob` component, no `falling`, no `FALL_TIME`.
    ///
    /// A regression here would not look like a bug. It would look like somebody making the
    /// window wait for the animation to finish "so it reads better", which is this client
    /// deciding a gameplay outcome — and the server would open the corpse anyway.
    #[test]
    fn interact_opens_a_corpse_on_the_tick_it_died_while_the_body_is_still_falling() {
        let mut buffer = SnapshotBuffer::default();
        // The tick before the blow: the same entity, alive, hunting, and lootable by
        // nobody.
        let mut alive = snapshot();
        alive.mobs[0].health = 60;
        alive.mobs[0].action = MobAction::Chase;
        alive.mobs[0].target_entity_id = PLAYER;
        alive.accessible_loot_corpses.clear();
        assert!(buffer.accept(alive, Instant::now()));
        // The tick of the blow, which is the tick the corpse exists on.
        let mut killed = snapshot();
        killed.server_tick = 2;
        assert!(buffer.accept(killed, Instant::now()));

        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(4);
        app.insert_resource(outbound)
            .insert_resource(ButtonInput::<KeyCode>::default())
            .insert_resource(buffer);
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
            }),
            "the loot window waited for an animation this side is not told the length of"
        );
    }

    /// **F in the loot window asks for everything, from the revision on screen.**
    ///
    /// One request, and it names neither an entry nor a count: which stacks fit is the
    /// server's answer, and the revision is what makes originating from a one-message-old
    /// view safe — a container that moved on since is refused rather than emptied blind.
    #[test]
    fn interact_inside_the_loot_window_asks_to_take_everything_shown() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(4);
        app.insert_resource(outbound)
            .insert_resource(ButtonInput::<KeyCode>::default());
        app.update();

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(6, 61)));
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Loot);
        // The open the window is already showing was never sent from this test, so
        // anything in the channel now is what this frame's key produced.
        assert!(frames.try_recv().is_err());

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyF);
        app.update();
        assert_eq!(
            frames.try_recv().unwrap(),
            encode_loot_take_all_request(&LootTakeAllRequest {
                corpse_id: CORPSE,
                revision: 6,
                client_tick: 0,
            })
        );
        assert!(
            frames.try_recv().is_err(),
            "the same press also asked to open a corpse"
        );
        assert_eq!(
            app.world().resource::<LootWindow>().state(),
            Some(&state(6, 61)),
            "the client removed an entry the server has not answered about"
        );
    }

    /// **A held F empties the corpse once**, the property #408 pinned for the open key,
    /// on the request the same key now also originates. A repeat frame that re-armed
    /// `just_pressed` would send a take-all per frame at a container that answers
    /// `StaleRevision` to all but the first — a burst of refusals for one press.
    #[test]
    fn holding_interact_inside_the_window_asks_to_take_everything_once() {
        const KEY: KeyCode = KeyCode::KeyF;
        let (mut app, frames) = held_key_app();
        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(6, 61)));
        app.update();
        assert!(frames.try_iter().collect::<Vec<_>>().is_empty());

        let mut sent = keyboard_frame(
            &mut app,
            &frames,
            [key_event(KEY, ButtonState::Pressed, false)],
        );
        for _ in 0..3 {
            sent.extend(keyboard_frame(
                &mut app,
                &frames,
                [key_event(KEY, ButtonState::Pressed, true)],
            ));
        }
        sent.extend(keyboard_frame(&mut app, &frames, []));
        let take_all = encode_loot_take_all_request(&LootTakeAllRequest {
            corpse_id: CORPSE,
            revision: 6,
            client_tick: 0,
        });
        assert_eq!(sent, vec![take_all.clone()], "a held key is one press");

        sent.extend(keyboard_frame(
            &mut app,
            &frames,
            [key_event(KEY, ButtonState::Released, false)],
        ));
        sent.extend(keyboard_frame(
            &mut app,
            &frames,
            [key_event(KEY, ButtonState::Pressed, false)],
        ));
        assert_eq!(
            sent,
            vec![take_all.clone(), take_all],
            "a press after a release is a second press"
        );
    }

    /// **A full pack leaves the window open; an emptied corpse closes it.**
    ///
    /// The two answers a take-all can come back as, from the side that has to keep
    /// showing the remainder. `RefusedAction::TakeLoot` moves nothing here — this client
    /// does not decide what came home — and the newest-revision guard accepts the
    /// remainder precisely because the server spent a revision on the entries that moved.
    #[test]
    fn a_refused_take_all_keeps_the_window_and_a_closure_ends_it() {
        let mut app = app();
        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(6, 61)));
        app.update();

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::State(state(7, 62)));
        app.update();
        assert_eq!(
            app.world().resource::<LootWindow>().state(),
            Some(&state(7, 62)),
            "the remainder of a partial take-all was refused as stale"
        );
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Loot);

        app.world_mut()
            .resource_mut::<LootInbox>()
            .push(LootEvent::Closed(LootClosed { corpse_id: CORPSE }));
        app.update();
        assert!(app.world().resource::<LootWindow>().state().is_none());
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
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

        // What `keyboard_input_system` does at the top of every frame, and what this test
        // has to do by hand because it inserts the resource rather than running the input
        // pipeline: `just_pressed` is per frame, and a press left armed would be read again
        // by the take-all branch the moment the window opens. The two held-key tests above
        // are where that edge is actually pinned, through the real plugin.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();

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
