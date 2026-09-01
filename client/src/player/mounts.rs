use bevy::prelude::*;

use super::{ApplyInputMode, ApplySnapshots, InputGate};
use crate::net::{
    LearnedMountsInbox, MountKind, MountRequest, Outbound, Session, encode_mount_request,
};
use crate::settings::{Control, DefaultMount, Settings};

#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct LearnedMounts(Vec<MountKind>);

impl LearnedMounts {
    pub fn mounts(&self) -> &[MountKind] {
        &self.0
    }

    pub fn contains(&self, mount: MountKind) -> bool {
        self.0.contains(&mount)
    }
}

#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct MountFeedback(Option<String>);

impl MountFeedback {
    pub fn line(&self) -> Option<&str> {
        self.0.as_deref()
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }

    fn show(&mut self, line: &str) {
        self.0 = Some(line.to_owned());
    }
}

pub(super) struct MountsPlugin;

impl Plugin for MountsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LearnedMounts>()
            .init_resource::<MountFeedback>()
            .init_resource::<LearnedMountsInbox>()
            .add_systems(
                Update,
                reconcile_learned_mounts
                    .after(crate::net::DrainNetwork)
                    .before(ApplySnapshots),
            )
            .add_systems(Update, send_mount_request.after(ApplyInputMode));
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
    mut feedback: ResMut<MountFeedback>,
    mut outbound: Option<ResMut<Outbound>>,
) {
    let bindings = settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());
    if !keys.is_some_and(|keys| keys.just_pressed(bindings.key(Control::Mount))) || !gate.may_act()
    {
        return;
    }
    if learned.0.is_empty() {
        feedback.show("You have not learned any mounts");
        return;
    }
    let Some(preference) = settings.and_then(|settings| settings.default_mount()) else {
        feedback.show("Choose a default mount in Inventory > Mounts");
        return;
    };
    let mount = mount_from_preference(preference);
    if !learned.contains(mount) {
        feedback.show("Your default mount is not learned on this character");
        return;
    }
    let Some(outbound) = outbound.as_deref_mut() else {
        return;
    };
    feedback.clear();
    outbound.send(encode_mount_request(&MountRequest { mount }));
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

pub const fn mount_label(mount: MountKind) -> &'static str {
    match mount {
        MountKind::BlackHorse => "Black horse",
        MountKind::BrownHorse => "Brown horse",
        MountKind::GreyHorse => "Grey horse",
    }
}
