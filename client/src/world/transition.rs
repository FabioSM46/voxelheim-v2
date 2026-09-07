//! An ordered replacement of the rendered world, declared only by the server.
//! The connection stays alive. The loading flag is a presentation gate, not a new
//! connection state, and no input or timer can start or complete a crossing.
use bevy::prelude::*;

use crate::net::{MapEvent, Session, WorldChange};
use crate::player::{InputMode, PlayerStats};

/// Identity is scoped to this connection; a seed alone never identifies a world.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub(crate) struct CurrentWorld {
    pub id: u64,
    pub seed: i64,
    pub exit_arch: Option<crate::net::BlockCoord>,
    pub loading: bool,
}

/// Applied as a command in DrainNetwork. Its automatic deferred-command flush
/// precedes every consumer ordered after that set, including a Welcome followed
/// immediately by WorldChange in the same network drain.
pub(crate) fn replace(world: &mut World, change: WorldChange, map: Vec<MapEvent>) {
    if !world.contains_resource::<Session>() {
        return;
    }
    crate::ui::replace_world_map(world, change, map);
    reset::<super::ChunkStore>(world);
    reset::<super::portal::PortalSites>(world);
    despawn::<super::portal::PortalVisual>(world);
    reset::<super::DecodeQueue>(world);
    super::render::reset_world(world);
    crate::player::reset_world(world);
    crate::audio::reset_world(world);
    crate::ui::reset_world(world);
    let mut session = world.resource_mut::<Session>();
    session.0.spawn = change.arrival;
    session.0.world_seed = change.world_seed;
    world.insert_resource(CurrentWorld {
        id: change.world_id,
        seed: change.world_seed,
        exit_arch: change.exit_arch,
        loading: true,
    });
    if let Some(mut mode) = world.get_resource_mut::<InputMode>() {
        *mode = InputMode::Menu;
    }
}

/// Connection-scoped identities and retained maps cannot outlive a welcome.
pub(crate) fn new_session(world: &mut World, seed: i64) {
    crate::ui::reset_world_maps(world);
    reset::<super::ChunkStore>(world);
    reset::<super::portal::PortalSites>(world);
    despawn::<super::portal::PortalVisual>(world);
    reset::<super::DecodeQueue>(world);
    super::render::reset_world(world);
    crate::player::reset_world(world);
    crate::audio::reset_world(world);
    crate::ui::reset_world(world);
    world.insert_resource(CurrentWorld { seed, ..default() });
}

/// Reset only installed plugins: focused headless consumers need not build the
/// entire client. Never create a second owner of a plugin's resources.
pub(crate) fn reset<T: Resource + Default>(world: &mut World) {
    if world.contains_resource::<T>() {
        world.insert_resource(T::default());
    }
}

pub(crate) fn clear_messages<T: Message>(world: &mut World) {
    if let Some(mut messages) = world.get_resource_mut::<Messages<T>>() {
        messages.clear();
    }
}

pub(crate) fn despawn<T: Component>(world: &mut World) {
    let entities: Vec<_> = world
        .query_filtered::<Entity, With<T>>()
        .iter(world)
        .collect();
    for entity in entities {
        if let Ok(entity) = world.get_entity_mut(entity) {
            entity.despawn();
        }
    }
}

pub(crate) struct TransitionPlugin;
impl Plugin for TransitionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CurrentWorld>()
            .add_systems(Startup, spawn_loading)
            // PostUpdate runs after the entire ingest/mesh/player schedule. A solid
            // voxel alone is insufficient: its mesh must have become an entity too.
            .add_systems(
                PostUpdate,
                (finish_loading, show_loading)
                    .chain()
                    .before(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate),
            );
    }
}

#[derive(Component)]
struct LoadingRoot;

#[derive(Component)]
struct LoadingText;

fn spawn_loading(mut commands: Commands) {
    commands
        .spawn((
            LoadingRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgb(0.025, 0.03, 0.04)),
            GlobalZIndex(1000),
            bevy::ui::FocusPolicy::Block,
            Visibility::Hidden,
        ))
        .with_children(|root| {
            root.spawn((
                LoadingText,
                Text::new("LOADING WORLD"),
                TextFont {
                    font_size: FontSize::Px(28.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

fn finish_loading(world: &mut World) {
    let Some(session) = world.get_resource::<Session>() else {
        let current = world.resource::<CurrentWorld>();
        if current.loading || current.id != 0 || current.seed != 0 {
            new_session(world, 0);
        }
        return;
    };
    if !world.resource::<CurrentWorld>().loading {
        return;
    }
    let size = usize::from(session.0.chunk_size);
    let position = world
        .get_resource::<PlayerStats>()
        .and_then(|stats| stats.position);
    let ready =
        position.is_some_and(|position| super::render::arrival_ready(world, position, size));
    if ready {
        world.resource_mut::<CurrentWorld>().loading = false;
        if let Some(mut mode) = world.get_resource_mut::<InputMode>() {
            *mode = InputMode::Playing;
        }
    }
}

fn show_loading(
    current: Res<CurrentWorld>,
    mut roots: Query<&mut Visibility, With<LoadingRoot>>,
    mut text: Query<&mut Text, With<LoadingText>>,
) {
    for mut line in &mut text {
        line.0 = if current.exit_arch.is_some() {
            "LOADING INSTANCE"
        } else {
            "LOADING OPEN WORLD"
        }
        .to_owned();
    }
    for mut visibility in &mut roots {
        *visibility = if current.loading {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}
