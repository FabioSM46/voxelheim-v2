//! The dungeon's levers and rune stones, as something the interact key can be pressed at.
//!
//! **Pressing it sends one `MechanismUseRequest` and decides nothing.** Whether the cell is
//! a mechanism, whether the player can reach it and whether its puzzle will let it move
//! are the server's (`Player.UseMechanism`: dead, not a mechanism, out of reach, locked —
//! in that order). A use that works is answered by the `BlockUpdate`s it causes, which the
//! world store applies and the mesher draws; one that does not is answered by an
//! `ActionRefused` the status line turns into a sentence. Nothing here changes a voxel,
//! predicts a lever's new state or remembers that one was pulled.
//!
//! **The prompt is a guess, and it is allowed to be wrong in one direction only.** The
//! server never says which cells are mechanisms, so the crosshair's voxel is read for the
//! shape a mechanism has in the drawing: a lever always, and a rune stone that stands
//! free — clear above it and on at least three of its four sides. That excludes the portal
//! arch's rune-stone frame and the inscription's lit marks set into a wall, both of which
//! the server would refuse as `NotAMechanism`. A press where the guess is wrong costs one
//! request and one refusal sentence, never an outcome.
//!
//! Aimed rather than nearest, like a station: the crosshair is on the lever, and
//! `target.rs` has already limited the ray to the reach the client aims with. The server
//! measures its own reach from the body to the cell, and that measure is the one that
//! counts.

use bevy::prelude::*;

use super::{ApplyInputMode, InputGate};
use crate::net::BlockCoord;
use crate::settings::{Control, Settings, key_name};
use crate::world::{BlockId, palette};

/// The mechanism under the crosshair, if the aimed voxel looks like one.
///
/// Written by `target.rs` in the same pass that finds the aimed voxel, because that pass
/// is the one that already reads the world store; this module reads only the answer.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AimedMechanism(pub Option<Aimed>);

/// One aimed mechanism: where it is and what it currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aimed {
    pub pos: BlockCoord,
    pub block: BlockId,
}

/// Whether `block` is one of the ids a dungeon mechanism shows, lit or not, thrown or not.
/// The client's mirror of the server's `world.Mechanism`.
pub(super) const fn is_mechanism(block: BlockId) -> bool {
    matches!(
        block,
        palette::LEVER_OFF | palette::LEVER_ON | palette::RUNE_STONE | palette::RUNE_STONE_LIT
    )
}

/// Whether `block` is a lever, thrown either way.
const fn is_lever(block: BlockId) -> bool {
    matches!(block, palette::LEVER_OFF | palette::LEVER_ON)
}

/// Whether the mechanism-shaped block at `pos` looks like one a player operates, reading
/// its neighbours through `block_at`. See the module comment for the shape and for why a
/// wrong guess is harmless.
pub(super) fn looks_operable(pos: IVec3, block_at: impl Fn(IVec3) -> BlockId) -> bool {
    let block = block_at(pos);
    if !is_mechanism(block) {
        return false;
    }
    if is_lever(block) {
        return true;
    }
    if palette::is_solid(block_at(pos + IVec3::Y)) {
        return false;
    }
    let open = [IVec3::X, IVec3::NEG_X, IVec3::Z, IVec3::NEG_Z]
        .into_iter()
        .filter(|side| !palette::is_solid(block_at(pos + *side)))
        .count();
    open >= 3
}

/// What the prompt says for `block`, with the interact key spelled as the player bound it.
pub(super) fn prompt(block: BlockId, key: KeyCode) -> Option<String> {
    let verb = if is_lever(block) {
        "Pull the lever"
    } else if is_mechanism(block) {
        "Touch the rune stone"
    } else {
        return None;
    };
    let key = key_name(key).map_or_else(|| "?".to_owned(), str::to_uppercase);
    Some(format!("[{key}] {verb}"))
}

#[derive(Component)]
struct MechanismHint;

pub(super) struct MechanismPlugin;

impl Plugin for MechanismPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AimedMechanism>()
            .add_systems(Startup, spawn_hint)
            .add_systems(
                Update,
                show_hint
                    .after(super::target::AimBlocks)
                    .after(ApplyInputMode),
            );
    }
}

fn spawn_hint(mut commands: Commands) {
    commands.spawn((
        MechanismHint,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(20.0),
            ..default()
        },
        TextColor(Color::srgb(0.93, 0.86, 0.66)),
        Node {
            position_type: PositionType::Absolute,
            // Above the portal's crossing hint, so the two never share a line.
            bottom: percent(30.0),
            width: percent(100.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        TextLayout::justify(Justify::Center),
        Visibility::Hidden,
        bevy::ui::FocusPolicy::Pass,
    ));
}

fn show_hint(
    gate: InputGate<'_>,
    aimed: Res<AimedMechanism>,
    settings: Option<Res<Settings>>,
    mut hints: Query<(&mut Text, &mut Visibility), With<MechanismHint>>,
) {
    let key = settings.as_deref().map_or_else(
        || KeyCode::KeyF,
        |settings| settings.bindings().key(Control::Interact),
    );
    let line = aimed
        .0
        .filter(|_| gate.may_aim())
        .and_then(|aimed| prompt(aimed.block, key));
    for (mut text, mut visibility) in &mut hints {
        let next = if line.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != next {
            *visibility = next;
        }
        if let Some(line) = &line
            && text.0 != *line
        {
            text.0.clone_from(line);
        }
    }
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<AimedMechanism>(world);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn world(cells: &[(IVec3, BlockId)]) -> impl Fn(IVec3) -> BlockId + use<> {
        let cells: HashMap<IVec3, BlockId> = cells.iter().copied().collect();
        move |pos| cells.get(&pos).copied().unwrap_or(palette::AIR)
    }

    #[test]
    fn the_four_mechanism_ids_are_the_servers_and_nothing_else_is() {
        // `world.Mechanism`: the unlit rune stone at 55 and the three ids #1288 appended.
        for block in [55, 61, 62, 63] {
            assert!(is_mechanism(block), "block {block}");
        }
        assert_eq!(
            (0..=u16::from(u8::MAX))
                .filter(|b| is_mechanism(*b))
                .count(),
            4
        );
        assert!(is_lever(61) && is_lever(62) && !is_lever(55) && !is_lever(63));
    }

    #[test]
    fn a_lever_always_looks_operable_and_a_free_standing_rune_stone_does() {
        let at = IVec3::new(4, 1, 7);
        let floor = |block| world(&[(at, block), (at - IVec3::Y, palette::STONE)]);
        for block in [
            palette::LEVER_OFF,
            palette::LEVER_ON,
            palette::RUNE_STONE,
            palette::RUNE_STONE_LIT,
        ] {
            assert!(looks_operable(at, floor(block)), "block {block}");
        }
        // A lever against a wall is still a lever.
        let walled = world(&[
            (at, palette::LEVER_OFF),
            (at + IVec3::X, palette::STONE),
            (at + IVec3::Z, palette::STONE),
            (at + IVec3::Y, palette::STONE),
        ]);
        assert!(looks_operable(at, walled));
        assert!(!looks_operable(at, world(&[(at, palette::STONE)])));
        assert!(!looks_operable(at, world(&[])));
    }

    #[test]
    fn a_portal_frame_and_an_inscription_in_a_wall_are_not_offered() {
        let at = IVec3::new(4, 3, 7);
        // One stone of an arch's frame: its neighbours along the frame are rune stone.
        let frame = world(&[
            (at, palette::RUNE_STONE),
            (at + IVec3::X, palette::RUNE_STONE),
            (at + IVec3::NEG_X, palette::RUNE_STONE),
        ]);
        assert!(!looks_operable(at, frame));
        // A lit mark set into a wall: stone above it and on either side.
        let mark = world(&[
            (at, palette::RUNE_STONE_LIT),
            (at + IVec3::Y, palette::BLACK_BRICK),
            (at + IVec3::X, palette::BLACK_BRICK),
            (at + IVec3::NEG_X, palette::BLACK_BRICK),
        ]);
        assert!(!looks_operable(at, mark));
    }

    #[test]
    fn the_prompt_names_the_bound_key_and_the_gesture() {
        assert_eq!(
            prompt(palette::LEVER_OFF, KeyCode::KeyF).as_deref(),
            Some("[F] Pull the lever")
        );
        assert_eq!(
            prompt(palette::LEVER_ON, KeyCode::KeyG).as_deref(),
            Some("[G] Pull the lever")
        );
        assert_eq!(
            prompt(palette::RUNE_STONE, KeyCode::KeyF).as_deref(),
            Some("[F] Touch the rune stone")
        );
        assert_eq!(
            prompt(palette::RUNE_STONE_LIT, KeyCode::KeyF).as_deref(),
            Some("[F] Touch the rune stone")
        );
        assert_eq!(prompt(palette::STONE, KeyCode::KeyF), None);
    }
}
