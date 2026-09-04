use bevy::prelude::*;

use super::items::{item_label, mount_item_id};
use super::{ApplyInputMode, ApplySnapshots, InputGate, LocalMount};
use crate::net::{
    LearnedMountsInbox, MountKind, MountRequest, Outbound, Session, encode_dismount_request,
    encode_mount_request,
};
use crate::settings::{Control, DefaultMount, Settings};
use crate::ui::{PlayerMessage, PlayerMessageKind, PublishPlayerMessages};

#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct LearnedMounts(Vec<MountKind>);

impl LearnedMounts {
    pub fn mounts(&self) -> &[MountKind] {
        &self.0
    }

    pub fn contains(&self, mount: MountKind) -> bool {
        self.0.contains(&mount)
    }

    #[cfg(test)]
    pub(crate) fn for_test(mounts: Vec<MountKind>) -> Self {
        Self(mounts)
    }
}

pub(super) struct MountsPlugin;

impl Plugin for MountsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LearnedMounts>()
            .init_resource::<LearnedMountsInbox>()
            // `PlayerPlugin` owns it in the game. Initialising it here keeps this
            // module's headless contract complete when it is built on its own.
            .init_resource::<LocalMount>()
            .add_message::<PlayerMessage>()
            .add_systems(
                Update,
                reconcile_learned_mounts
                    .after(crate::net::DrainNetwork)
                    .before(ApplySnapshots),
            )
            .add_systems(
                Update,
                send_mount_request
                    .after(ApplyInputMode)
                    .in_set(PublishPlayerMessages),
            );
    }
}

fn reconcile_learned_mounts(
    mut inbox: ResMut<LearnedMountsInbox>,
    session: Option<Res<Session>>,
    mut learned: ResMut<LearnedMounts>,
) {
    if session.is_none() {
        if !learned.0.is_empty() {
            learned.0.clear();
        }
        inbox.take();
        return;
    }
    if let Some(newest) = inbox.take().into_iter().last()
        && learned.0 != newest.mounts
    {
        learned.0 = newest.mounts;
    }
}

fn send_mount_request(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    settings: Option<Res<Settings>>,
    gate: InputGate<'_>,
    local_mount: Res<LocalMount>,
    learned: Res<LearnedMounts>,
    mut messages: MessageWriter<PlayerMessage>,
    mut outbound: Option<ResMut<Outbound>>,
) {
    let bindings = settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());
    if !keys.is_some_and(|keys| keys.just_pressed(bindings.key(Control::Mount))) || !gate.may_act()
    {
        return;
    }
    if local_mount.mounted() {
        // The authoritative snapshot already says this session is mounted: send the
        // dismount intent and nothing else. `Player.Dismount` in
        // server/internal/game/mount.go is immediate and unconditional, so there is no
        // precondition to resolve here — not the default-mount preference, not the
        // learned set — and no local prediction: nothing here clears `LocalMount`, moves
        // the eye or removes the horse view. The ride ends when the mount entry leaves
        // the next snapshot.
        //
        // A press while a mount cast is still running does not reach this branch: the
        // server has not added the mount entry yet, so `LocalMount::mounted` still
        // answers `false` and the press falls through to the mount arm below exactly as
        // it does today. The server already handles that case with
        // `RefusalReasonAlreadyMounted` or a cancel per `Player.Dismount` — inventing a
        // client-side rule for it here would be the bug this branch exists to avoid.
        let Some(outbound) = outbound.as_deref_mut() else {
            return;
        };
        outbound.send(encode_dismount_request());
        return;
    }
    let mount = match requested_mount(
        &learned,
        settings.and_then(|settings| settings.default_mount()),
    ) {
        Ok(mount) => mount,
        Err(line) => {
            messages.write(PlayerMessage::new(PlayerMessageKind::Warn, line));
            return;
        }
    };
    let Some(outbound) = outbound.as_deref_mut() else {
        return;
    };
    outbound.send(encode_mount_request(&MountRequest { mount }));
}

fn requested_mount(
    learned: &LearnedMounts,
    preference: Option<DefaultMount>,
) -> Result<MountKind, &'static str> {
    if learned.0.is_empty() {
        return Err("You have not learned any mounts.");
    }
    let Some(preference) = preference else {
        return Err("Choose a default mount in Inventory > Mounts.");
    };
    let mount = mount_from_preference(preference);
    if !learned.contains(mount) {
        return Err("Your default mount is not learned on this character.");
    }
    Ok(mount)
}

pub const fn mount_from_preference(preference: DefaultMount) -> MountKind {
    match preference {
        DefaultMount::Black => MountKind::BlackHorse,
        DefaultMount::Brown => MountKind::BrownHorse,
        DefaultMount::Grey => MountKind::GreyHorse,
    }
}

pub const fn preference_from_mount(mount: MountKind) -> DefaultMount {
    match mount {
        MountKind::BlackHorse => DefaultMount::Black,
        MountKind::BrownHorse => DefaultMount::Brown,
        MountKind::GreyHorse => DefaultMount::Grey,
    }
}

pub fn mount_label(mount: MountKind) -> &'static str {
    item_label(mount_item_id(mount))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, SessionParams};
    use crate::player::{InputMode, SelfVitals, ViewMode};

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
            voice_range_blocks: 0.0,
        })
    }

    #[test]
    fn every_local_mount_precondition_has_the_requested_warning() {
        assert_eq!(
            requested_mount(&LearnedMounts(Vec::new()), None),
            Err("You have not learned any mounts.")
        );
        assert_eq!(
            requested_mount(&LearnedMounts(vec![MountKind::BlackHorse]), None),
            Err("Choose a default mount in Inventory > Mounts.")
        );
        assert_eq!(
            requested_mount(
                &LearnedMounts(vec![MountKind::BlackHorse]),
                Some(DefaultMount::Grey)
            ),
            Err("Your default mount is not learned on this character.")
        );
    }

    #[test]
    fn learned_and_default_mount_rows_share_the_item_registry_names() {
        for mount in [
            MountKind::BlackHorse,
            MountKind::BrownHorse,
            MountKind::GreyHorse,
        ] {
            assert_eq!(mount_label(mount), item_label(mount_item_id(mount)));
        }
        assert_eq!(mount_label(MountKind::BlackHorse), "Raven Friesian");
        assert_eq!(mount_label(MountKind::BrownHorse), "Chestnut Icelandic");
        assert_eq!(mount_label(MountKind::GreyHorse), "Silver Fjord");
    }

    /// A headless [`MountsPlugin`] app with the resources [`InputGate`] and
    /// `send_mount_request` read, none of which `MountsPlugin` owns itself — the same
    /// contract `loot.rs` and `combat.rs` build for their own gated systems. A session is
    /// present, exactly as it is whenever `Control::Mount` is reachable in play: absent
    /// one, `reconcile_learned_mounts` clears `LearnedMounts` back to empty on every
    /// frame, which would silently undo whatever a test inserted. The first
    /// [`App::update`] settles the resources' initial change-detection flags, exactly as
    /// `player/combat.rs`'s `clicking_app` does, so a later press is the only thing that
    /// makes [`InputGate::may_act`] see a change.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .insert_resource(session())
            .insert_resource(ButtonInput::<KeyCode>::default())
            .add_plugins(MountsPlugin);
        app.update();
        app
    }

    /// Presses the default binding for [`Control::Mount`] — `KeyZ`, and this issue forbids
    /// touching `Control`/`CONTROLS`, so a settings-sourced binding is out of scope here.
    fn press_mount(app: &mut App) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyZ);
    }

    /// Every [`Settings`] a preference-resolution test needs, with one default mount.
    fn settings_preferring(mount: MountKind) -> Settings {
        let mut settings = Settings::default();
        settings.set_default_mount(preference_from_mount(mount));
        settings
    }

    fn player_messages(app: &App) -> Vec<PlayerMessage> {
        let messages = app.world().resource::<Messages<PlayerMessage>>();
        let mut cursor = messages.get_cursor();
        cursor.read(messages).cloned().collect()
    }

    #[test]
    fn mounted_press_sends_exactly_one_dismount_request_and_no_mount_request() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(4);
        app.insert_resource(outbound)
            .insert_resource(LocalMount::from_server(Some(MountKind::BlackHorse)))
            .insert_resource(LearnedMounts::for_test(vec![MountKind::BlackHorse]))
            .insert_resource(settings_preferring(MountKind::BlackHorse));
        app.update();

        press_mount(&mut app);
        app.update();

        assert_eq!(
            frames.try_recv().unwrap(),
            encode_dismount_request(),
            "a mounted press must send the dismount intent"
        );
        assert!(
            frames.try_recv().is_err(),
            "one press must send exactly one message"
        );
    }

    /// Today's unmounted behaviour, unchanged: the default-mount preference is resolved
    /// and a `MountRequest` is sent for it.
    #[test]
    fn unmounted_press_still_resolves_the_default_mount_and_sends_a_mount_request() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(4);
        app.insert_resource(outbound)
            .insert_resource(LearnedMounts::for_test(vec![MountKind::BlackHorse]))
            .insert_resource(settings_preferring(MountKind::BlackHorse));
        app.update();

        press_mount(&mut app);
        app.update();

        assert_eq!(
            frames.try_recv().unwrap(),
            encode_mount_request(&MountRequest {
                mount: MountKind::BlackHorse
            })
        );
        assert!(
            frames.try_recv().is_err(),
            "one press must send exactly one message"
        );
    }

    /// **Dismount asks for no preconditions the mount path asks for.** No default mount
    /// selected (no [`Settings`] resource at all) and an empty learned set would both
    /// refuse the mount arm with a warning line; the mounted arm must read neither.
    #[test]
    fn mounted_press_asks_no_preconditions_and_writes_no_warning() {
        let mut app = app();
        let (outbound, frames) = Outbound::to_a_test(4);
        app.insert_resource(outbound)
            .insert_resource(LocalMount::from_server(Some(MountKind::BlackHorse)));
        app.update();

        press_mount(&mut app);
        app.update();

        assert_eq!(frames.try_recv().unwrap(), encode_dismount_request());
        assert!(frames.try_recv().is_err());
        assert!(
            player_messages(&app).is_empty(),
            "a mounted press must not be refused for a missing default or an empty learned set"
        );
    }

    #[test]
    fn a_closed_input_gate_sends_nothing_in_either_direction() {
        for local_mount in [
            LocalMount::default(),
            LocalMount::from_server(Some(MountKind::BlackHorse)),
        ] {
            let mut app = app();
            let (outbound, frames) = Outbound::to_a_test(4);
            app.insert_resource(outbound).insert_resource(local_mount);
            app.update();

            // Changed in the same frame as the press, exactly as
            // `combat.rs::a_ui_mode_or_a_death_suppresses_the_swing` closes the gate: the
            // mode leaving `Playing` is what `InputGate::may_act` reads either way.
            *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
            press_mount(&mut app);
            app.update();

            assert!(
                frames.try_recv().is_err(),
                "mounted={} sent something while the gate was closed",
                local_mount.mounted()
            );
        }
    }

    #[test]
    fn no_press_sends_nothing_whether_mounted_or_not() {
        for local_mount in [
            LocalMount::default(),
            LocalMount::from_server(Some(MountKind::BlackHorse)),
        ] {
            let mut app = app();
            let (outbound, frames) = Outbound::to_a_test(4);
            app.insert_resource(outbound).insert_resource(local_mount);
            app.update();
            app.update();

            assert!(
                frames.try_recv().is_err(),
                "mounted={} sent something with no press",
                local_mount.mounted()
            );
        }
    }
}
