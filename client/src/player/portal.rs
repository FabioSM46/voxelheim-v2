//! The nearby crossing hint. A portal is crossed by walking into its veil, which the server
//! notices on its own: nothing here sends anything, and no key is involved.
use super::{ApplyInputMode, ApplySnapshots, InputGate, SnapshotBuffer};
use crate::net::{BlockCoord, Session};
use crate::world::{portal::PortalSites, transition::CurrentWorld};
use bevy::prelude::*;

#[derive(Resource, Default)]
pub(super) struct PortalFocus(pub Option<BlockCoord>);
#[derive(Component)]
struct PortalHint;

/// Guidance names the gesture, never a key: the veil is crossed by walking into it.
const PORTAL_ENTER_HINT: &str = "Walk into the veil to enter the dungeon";
const PORTAL_RETURN_HINT: &str = "Walk into the veil to return";

pub(super) struct PortalInteractionPlugin;
impl Plugin for PortalInteractionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PortalFocus>()
            .add_systems(Startup, spawn_hint)
            .add_systems(
                Update,
                focus_portal
                    .after(crate::world::portal::PortalUpdate)
                    .after(ApplyInputMode)
                    .after(ApplySnapshots),
            );
    }
}

fn spawn_hint(mut commands: Commands) {
    commands.spawn((
        PortalHint,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(20.0),
            ..default()
        },
        TextColor(Color::srgb(0.40, 0.95, 0.79)),
        Node {
            position_type: PositionType::Absolute,
            bottom: percent(25.0),
            width: percent(100.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        TextLayout::justify(Justify::Center),
        Visibility::Hidden,
        bevy::ui::FocusPolicy::Pass,
    ));
}

#[derive(bevy::ecs::system::SystemParam)]
struct FocusContext<'w> {
    gate: InputGate<'w>,
    session: Option<Res<'w, Session>>,
    buffer: Res<'w, SnapshotBuffer>,
    sites: Option<Res<'w, PortalSites>>,
    current: Option<Res<'w, CurrentWorld>>,
}

fn focus_portal(
    context: FocusContext,
    mut focus: ResMut<PortalFocus>,
    mut hints: Query<(&mut Text, &mut Visibility), With<PortalHint>>,
) {
    let FocusContext {
        gate,
        session,
        buffer,
        sites,
        current,
    } = context;
    focus.0 = None;
    if gate.may_aim()
        && !current.as_deref().is_some_and(|w| w.loading)
        && let (Some(session), Some(sites)) = (session, sites)
        && buffer.mount_of(session.0.entity_id).is_none()
        && let Some(player) = buffer.latest_snapshot().and_then(|s| {
            s.entities
                .iter()
                .find(|e| e.entity_id == session.0.entity_id)
        })
    {
        focus.0 = sites.nearest(player.pos);
        // In an instance, only its server-declared return anchor is actionable.
        if current
            .as_deref()
            .is_some_and(|w| w.id != 0 && focus.0 != w.exit_arch)
        {
            focus.0 = None;
        }
    }
    for (mut text, mut visibility) in &mut hints {
        *visibility = if focus.0.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if focus.0.is_none() {
            continue;
        }
        let next = if current.as_deref().is_some_and(|w| w.id != 0) {
            PORTAL_RETURN_HINT
        } else {
            PORTAL_ENTER_HINT
        };
        if text.0 != next {
            text.0 = next.to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, EntityState, SessionParams, Snapshot};
    use crate::player::{InputMode, SelfVitals, ViewMode};
    use crate::world::portal::PortalSite;
    #[test]
    fn focus_requires_live_play_and_the_instances_declared_exit() {
        let arch = BlockCoord { x: 8, y: 2, z: 8 };
        let mut sites = PortalSites::default();
        sites.sites.push(PortalSite {
            arch,
            bottom: Vec3::new(8.5, 1.0, 8.5),
            along_z: false,
        });
        let mut buffer = SnapshotBuffer::default();
        buffer.accept(
            Snapshot {
                server_tick: 1,
                entities: vec![EntityState {
                    entity_id: 7,
                    pos: [8.5, 1.0, 10.0],
                    vel: [0.0; 3],
                    yaw: 0.0,
                }],
                ..default()
            },
            std::time::Instant::now(),
        );
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(sites)
            .insert_resource(buffer)
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .init_resource::<PortalFocus>()
            .init_resource::<CurrentWorld>()
            .insert_resource(Session(SessionParams {
                clock: default(),
                entity_id: 7,
                spawn: [8.5, 1.0, 10.0],
                world_seed: 1,
                tick_rate: 20,
                chunk_size: 32,
                view_distance: 8,
                inventory_slots: 37,
                hotbar_slots: 9,
                equipment_slots: 4,
                player_token: ANY_TOKEN,
                voice_range_blocks: 0.0,
            }))
            .add_systems(Startup, spawn_hint)
            .add_systems(Update, focus_portal);
        app.update();
        assert_eq!(app.world().resource::<PortalFocus>().0, Some(arch));
        let hint = |app: &mut App| {
            let mut hints = app.world_mut().query::<(&Text, &Visibility)>();
            let (text, visibility) = hints.single(app.world()).unwrap();
            (text.0.clone(), *visibility)
        };
        assert_eq!(
            hint(&mut app),
            (PORTAL_ENTER_HINT.to_owned(), Visibility::Visible)
        );
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
        app.update();
        assert_eq!(app.world().resource::<PortalFocus>().0, None);
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.world_mut().resource_mut::<CurrentWorld>().loading = true;
        app.update();
        assert_eq!(app.world().resource::<PortalFocus>().0, None);
        app.insert_resource(CurrentWorld {
            id: 9,
            exit_arch: Some(BlockCoord { x: 1, y: 1, z: 1 }),
            ..default()
        });
        app.update();
        assert_eq!(app.world().resource::<PortalFocus>().0, None);
        app.world_mut().resource_mut::<CurrentWorld>().exit_arch = Some(arch);
        app.update();
        assert_eq!(app.world().resource::<PortalFocus>().0, Some(arch));
        assert_eq!(
            hint(&mut app),
            (PORTAL_RETURN_HINT.to_owned(), Visibility::Visible)
        );
        app.world_mut().remove_resource::<Session>();
        app.update();
        assert_eq!(app.world().resource::<PortalFocus>().0, None);
        assert_eq!(hint(&mut app).1, Visibility::Hidden);
        app.world_mut().resource_mut::<PortalFocus>().0 = Some(arch);
        crate::player::reset_world(app.world_mut());
        assert_eq!(app.world().resource::<PortalFocus>().0, None);
    }
}
