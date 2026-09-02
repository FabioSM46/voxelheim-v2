//! The reusable local yes/no confirmation.
//!
//! A controller supplies only a title, an opaque token and the mode to return to. This
//! module draws the shared frame, turns either button into a typed answer and knows
//! nothing about what accepting asks for.

use bevy::prelude::*;

use super::{BUTTON, button_colour};
use crate::player::{ApplyInputMode, ConfirmationAnswer, ConfirmationPrompt, InputMode};

const WIDTH: f32 = 360.0;

#[derive(Component)]
struct PromptRoot;

#[derive(Component, Debug, Clone, Copy)]
struct PromptButton(bool);

type ChangedPromptButtons<'w, 's> = Query<
    'w,
    's,
    (
        &'static Interaction,
        &'static PromptButton,
        &'static mut BackgroundColor,
    ),
    (Changed<Interaction>, With<Button>),
>;

pub(super) struct ConfirmationPromptUiPlugin;

impl Plugin for ConfirmationPromptUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ConfirmationPrompt>()
            .init_resource::<InputMode>()
            .add_message::<ConfirmationAnswer>()
            .add_systems(Startup, spawn_prompt)
            .add_systems(
                Update,
                (rebuild_prompt, click_prompt, show_prompt)
                    .chain()
                    .before(ApplyInputMode),
            );
    }
}

fn spawn_prompt(mut commands: Commands) {
    commands.spawn((
        PromptRoot,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(WIDTH),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(14.0),
            padding: UiRect::all(Val::Px(16.0)),
            ..default()
        },
        UiTransform::from_translation(Val2::percent(-50.0, -50.0)),
        // The loot, vendor and player-trade frame and ink, exactly.
        BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.96)),
        GlobalZIndex(30),
        Visibility::Hidden,
    ));
}

fn rebuild_prompt(
    prompt: Res<ConfirmationPrompt>,
    roots: Query<Entity, With<PromptRoot>>,
    mut commands: Commands,
) {
    if !prompt.is_changed() {
        return;
    }
    for root in &roots {
        commands.entity(root).despawn_related::<Children>();
        let Some(current) = prompt.current() else {
            continue;
        };
        commands.entity(root).with_children(|root| {
            root.spawn((
                Text::new(current.title().to_owned()),
                TextFont {
                    font_size: FontSize::Px(22.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            root.spawn(Node {
                width: Val::Percent(100.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::FlexEnd,
                column_gap: Val::Px(8.0),
                ..default()
            })
            .with_children(|buttons| {
                spawn_button(buttons, "No", false);
                spawn_button(buttons, "Yes", true);
            });
        });
    }
}

fn spawn_button(buttons: &mut ChildSpawnerCommands<'_>, label: &str, accepted: bool) {
    buttons
        .spawn((
            PromptButton(accepted),
            Button,
            Node {
                padding: UiRect::axes(Val::Px(14.0), Val::Px(7.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
        ))
        .with_children(|button| {
            button.spawn((
                Text::new(label.to_owned()),
                TextFont {
                    font_size: FontSize::Px(16.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

fn click_prompt(
    mut buttons: ChangedPromptButtons<'_, '_>,
    mut prompt: ResMut<ConfirmationPrompt>,
    mut mode: ResMut<InputMode>,
    mut answers: MessageWriter<ConfirmationAnswer>,
) {
    for (interaction, button, mut colour) in &mut buttons {
        *colour = button_colour(interaction).into();
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some((answer, return_mode)) = prompt.answer(button.0) else {
            continue;
        };
        super::set_mode(&mut mode, return_mode);
        answers.write(answer);
    }
}

fn show_prompt(
    prompt: Res<ConfirmationPrompt>,
    mode: Res<InputMode>,
    mut roots: Query<&mut Visibility, With<PromptRoot>>,
) {
    let visible = *mode == InputMode::TradePrompt && prompt.current().is_some();
    for mut visibility in &mut roots {
        let next = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != next {
            *visibility = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_widget_draws_the_shared_frame_title_and_two_answers() {
        let mut app = App::new();
        app.add_plugins(ConfirmationPromptUiPlugin);
        app.world_mut()
            .resource_mut::<ConfirmationPrompt>()
            .open("Trade with Freya?".to_owned(), InputMode::Playing);
        *app.world_mut().resource_mut::<InputMode>() = InputMode::TradePrompt;
        app.update();

        let world = app.world_mut();
        let root = world
            .query_filtered::<(&BackgroundColor, &GlobalZIndex, &Visibility), With<PromptRoot>>()
            .single(world)
            .expect("one prompt root");
        assert_eq!(root.0.0, Color::srgba(0.025, 0.03, 0.04, 0.96));
        assert_eq!(*root.1, GlobalZIndex(30));
        assert_eq!(*root.2, Visibility::Visible);

        let answers: Vec<bool> = world
            .query::<&PromptButton>()
            .iter(world)
            .map(|button| button.0)
            .collect();
        assert_eq!(answers, [false, true]);
        assert!(
            world
                .query::<&Text>()
                .iter(world)
                .any(|text| text.0 == "Trade with Freya?")
        );
    }
}
