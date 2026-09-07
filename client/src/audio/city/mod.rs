//! Positional forge and campfire ambience from authoritative structure snapshots.
mod sound;

mod runtime;

pub(super) fn register(app: &mut bevy::prelude::App) {
    runtime::register(app);
}
