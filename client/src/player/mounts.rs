use bevy::prelude::*;

use super::items::{item_label, mount_item_id};
use super::{ApplyInputMode, ApplySnapshots, InputGate};
use crate::net::{
    LearnedMountsInbox, MountKind, MountRequest, Outbound, Session, encode_mount_request,
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
}
