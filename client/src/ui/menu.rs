//! The three-action pause menu.

use bevy::prelude::*;

use super::login::login_is_up;
use super::set_mode;
use super::{BUTTON, button_colour};
use crate::net::{DisconnectRequest, Session, SignInState};
use crate::player::InputMode;

pub(super) struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_menu)
            .add_systems(Update, (show_menu, menu_actions));
    }
}

#[derive(Component)]
struct MenuRoot;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    Resume,
    Disconnect,
    Quit,
}

type MenuButton<'a> = (&'a Interaction, &'a MenuAction, &'a mut BackgroundColor);
type ChangedMenuButton = (Changed<Interaction>, With<Button>);

fn spawn_menu(mut commands: Commands) {
    commands
        .spawn((
            MenuRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.012, 0.016, 0.024, 0.96)),
            Visibility::Hidden,
            GlobalZIndex(40),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        width: Val::Px(340.0),
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(12.0),
                        padding: UiRect::all(Val::Px(28.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.065, 0.075, 0.095)),
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new("PAUSED"),
                        TextFont {
                            font_size: FontSize::Px(30.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                        Node {
                            align_self: AlignSelf::Center,
                            margin: UiRect {
                                bottom: Val::Px(8.0),
                                ..default()
                            },
                            ..default()
                        },
                    ));
                    spawn_button(panel, MenuAction::Resume, "RESUME");
                    spawn_button(panel, MenuAction::Disconnect, "DISCONNECT");
                    spawn_button(panel, MenuAction::Quit, "QUIT");
                });
        });
}

fn spawn_button(parent: &mut ChildSpawnerCommands<'_>, action: MenuAction, label: &str) {
    parent
        .spawn((
            action,
            Button,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(48.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
        ))
        .with_child((
            Text::new(label),
            TextFont {
                font_size: FontSize::Px(20.0),
                ..default()
            },
            TextColor(Color::WHITE),
            TextShadow::default(),
        ));
}

fn show_menu(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    sign_in: Option<Res<SignInState>>,
    mut roots: Query<&mut Visibility, With<MenuRoot>>,
) {
    // The login screen puts the input mode in `Menu` to take a click away from the
    // world, and this is the other half of that: the pause menu is not what that
    // mode is about while a player has not signed in, and drawing it underneath
    // would be two panels for one state.
    let next = if *mode == InputMode::Menu && session.is_some() && !login_is_up(sign_in.as_deref())
    {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut roots {
        if *visibility != next {
            *visibility = next;
        }
    }
}

fn menu_actions(
    mut buttons: Query<MenuButton<'_>, ChangedMenuButton>,
    mut mode: ResMut<InputMode>,
    mut disconnect: MessageWriter<DisconnectRequest>,
    mut exit: MessageWriter<AppExit>,
) {
    for (interaction, action, mut colour) in &mut buttons {
        colour.0 = button_colour(interaction);

        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            MenuAction::Resume => set_mode(&mut mode, InputMode::Playing),
            MenuAction::Disconnect => {
                disconnect.write(DisconnectRequest);
                set_mode(&mut mode, InputMode::Playing);
            }
            MenuAction::Quit => {
                exit.write(AppExit::Success);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::SessionParams;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 4,
            hotbar_slots: 4,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    #[test]
    fn disconnect_is_one_menu_action_and_returns_to_the_status_view() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<DisconnectRequest>()
            .add_message::<AppExit>()
            .insert_resource(session())
            .insert_resource(InputMode::Menu)
            .add_plugins(MenuPlugin);
        app.update();

        let disconnect_button = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &MenuAction)>();
            let actions: Vec<(Entity, MenuAction)> = query
                .iter(world)
                .map(|(entity, action)| (entity, *action))
                .collect();
            assert_eq!(actions.len(), 3, "the pause menu has exactly three entries");
            actions
                .into_iter()
                .find(|(_, action)| *action == MenuAction::Disconnect)
                .map(|(entity, _)| entity)
                .expect("disconnect entry")
        };
        *app.world_mut()
            .entity_mut(disconnect_button)
            .get_mut::<Interaction>()
            .expect("a button has an interaction") = Interaction::Pressed;
        app.update();

        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<DisconnectRequest>>()
                .drain()
                .count(),
            1
        );
    }
}
