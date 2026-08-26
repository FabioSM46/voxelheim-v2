//! Dedicated view of one complete server-owned corpse-container revision.

use bevy::prelude::*;

use super::{BUTTON, CELL_SIZE, FILLED_CELL, button_colour, icon, stack_style};
use crate::net::{InventoryStack, Session};
use crate::player::{InputMode, LootTakeClick, LootWindow, item_label};

const WIDTH: f32 = 430.0;

#[derive(Component)]
struct LootRoot;

#[derive(Component)]
struct LootEntryButton(u64);

pub(super) struct LootUiPlugin;

impl Plugin for LootUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LootWindow>()
            .init_resource::<InputMode>()
            .add_message::<LootTakeClick>()
            .add_systems(Startup, spawn_window)
            .add_systems(Update, (rebuild_window, show_window, click_entries));
    }
}

fn spawn_window(mut commands: Commands) {
    commands.spawn((
        LootRoot,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(WIDTH),
            max_height: Val::Percent(80.0),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(8.0),
            padding: UiRect::all(Val::Px(14.0)),
            overflow: Overflow::scroll_y(),
            ..default()
        },
        UiTransform::from_translation(Val2::percent(-50.0, -50.0)),
        BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.96)),
        GlobalZIndex(30),
        Visibility::Hidden,
    ));
}

fn rebuild_window(
    window: Res<LootWindow>,
    roots: Query<Entity, With<LootRoot>>,
    mut commands: Commands,
) {
    if !window.is_changed() {
        return;
    }
    for root in &roots {
        commands.entity(root).despawn_related::<Children>();
        let Some(state) = window.state() else {
            continue;
        };
        commands.entity(root).with_children(|root| {
            root.spawn((
                Text::new(format!("Loot · revision {}", state.revision)),
                TextFont {
                    font_size: FontSize::Px(22.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            for entry in &state.entries {
                let durability = if entry.max_durability == 0 {
                    String::new()
                } else {
                    format!(
                        " · durability {}/{}",
                        entry.durability, entry.max_durability
                    )
                };
                let style = stack_style(Some(InventoryStack {
                    item_id: entry.item_id,
                    count: entry.count,
                    durability: entry.durability,
                    max_durability: entry.max_durability,
                }));
                root.spawn((
                    LootEntryButton(entry.entry_id),
                    Button,
                    Node {
                        width: Val::Percent(100.0),
                        min_height: Val::Px(CELL_SIZE + 8.0),
                        display: Display::Flex,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(10.0),
                        padding: UiRect::all(Val::Px(4.0)),
                        ..default()
                    },
                    BackgroundColor(BUTTON),
                ))
                .with_children(|row| {
                    row.spawn((
                        Node {
                            width: Val::Px(CELL_SIZE),
                            height: Val::Px(CELL_SIZE),
                            position_type: PositionType::Relative,
                            ..default()
                        },
                        BackgroundColor(FILLED_CELL),
                    ))
                    .with_children(|host| {
                        if let Some(icon) = style.icon {
                            icon::spawn(host, icon);
                        }
                    });
                    row.spawn((
                        Text::new(format!(
                            "{} x {}{durability}",
                            item_label(entry.item_id),
                            entry.count
                        )),
                        TextFont {
                            font_size: FontSize::Px(16.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });
            }
        });
    }
}

fn show_window(
    window: Res<LootWindow>,
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Visibility, With<LootRoot>>,
) {
    let shown = session.is_some() && *mode == InputMode::Loot && window.state().is_some();
    for mut visibility in &mut roots {
        *visibility = if shown {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

fn click_entries(
    mut entries: Query<
        (&LootEntryButton, &Interaction, &mut BackgroundColor),
        Changed<Interaction>,
    >,
    mut clicks: MessageWriter<LootTakeClick>,
) {
    for (entry, interaction, mut colour) in &mut entries {
        colour.0 = button_colour(interaction);
        if *interaction == Interaction::Pressed {
            clicks.write(LootTakeClick(entry.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, LootEntry, LootState, SessionParams};

    #[test]
    fn the_window_draws_complete_entries_and_a_click_only_emits_the_entry_id() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Session(SessionParams {
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
            }))
            .insert_resource(InputMode::Loot)
            .insert_resource(LootWindow::from_server(LootState {
                corpse_id: 90,
                revision: 4,
                entries: vec![LootEntry {
                    entry_id: 44,
                    item_id: 1,
                    count: 3,
                    durability: 7,
                    max_durability: 10,
                }],
            }))
            .add_plugins(LootUiPlugin);
        app.update();

        let world = app.world_mut();
        let mut roots = world.query_filtered::<&Visibility, With<LootRoot>>();
        assert_eq!(roots.single(world).unwrap(), &Visibility::Visible);
        let mut texts = world.query::<&Text>();
        let lines: Vec<_> = texts.iter(world).map(|text| text.0.as_str()).collect();
        assert!(lines.contains(&"Loot · revision 4"));
        assert!(
            lines
                .iter()
                .any(|line| { line.contains("x 3") && line.contains("durability 7/10") })
        );

        let mut buttons = world.query::<(&LootEntryButton, &mut Interaction)>();
        let (entry, mut interaction) = buttons.single_mut(world).expect("one loot entry");
        assert_eq!(entry.0, 44);
        *interaction = Interaction::Pressed;
        app.update();
        let clicks: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<LootTakeClick>>()
            .drain()
            .collect();
        assert_eq!(clicks, [LootTakeClick(44)]);
        assert_eq!(
            app.world()
                .resource::<LootWindow>()
                .state()
                .unwrap()
                .entries
                .len(),
            1
        );
    }
}
