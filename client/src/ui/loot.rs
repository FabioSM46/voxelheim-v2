//! Dedicated view of one complete server-owned corpse-container revision.

use bevy::prelude::*;

use super::{BUTTON, CELL_SIZE, FILLED_CELL, button_colour, icon, silver_icon, stack_style};
use crate::net::{InventoryStack, Session};
use crate::player::{InputMode, Liveries, LootTakeClick, LootWindow, item_label};

const WIDTH: f32 = 430.0;

/// The label the take-all line names, kept beside the window that prints it.
///
/// It is the *default* binding rather than the bound one: `Settings` is a resource this
/// module deliberately does not read, and a hint is presentation. A rebound interact key
/// makes this line wrong, which is a smaller cost than the UI growing an opinion about
/// input — and the issue that rebinds it is the one that should fix it here.
const TAKE_ALL_KEY: &str = "F";

/// Dimmer than the entries, because it is an instruction rather than a thing to click.
const HINT: Color = Color::srgb(0.62, 0.62, 0.66);

#[derive(Component)]
struct LootRoot;

#[derive(Component)]
struct LootEntryButton(u64);

/// The corpse's server-owned currency, drawn apart from its clickable item entries.
#[derive(Component)]
struct LootSilver;

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
    // Optional, because the UI stands up headlessly without the player plugin that owns it.
    liveries: Option<Res<Liveries>>,
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
                Text::new(format!("Loot | revision {}", state.revision)),
                TextFont {
                    font_size: FontSize::Px(22.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            // The second gesture the window answers to, written down because nothing else
            // says it: interact opened this, and interact again empties it.
            //
            // ASCII, and the hyphen is deliberate. Bevy's `default_font` is the whole font
            // stack here — a 95-glyph subset of FiraMono — so an em dash is a glyph it
            // does not have and would render as nothing at all, which on a line whose only
            // job is to teach a control is the one failure that hides itself.
            root.spawn((
                Text::new(format!("{} - take all", TAKE_ALL_KEY)),
                TextFont {
                    font_size: FontSize::Px(14.0),
                    ..default()
                },
                TextColor(HINT),
            ));
            if state.silver > 0 {
                root.spawn((
                    LootSilver,
                    Node {
                        width: Val::Percent(100.0),
                        min_height: Val::Px(CELL_SIZE),
                        display: Display::Flex,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(10.0),
                        padding: UiRect::all(Val::Px(4.0)),
                        ..default()
                    },
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
                    .with_children(|host| icon::spawn(host, silver_icon(), None));
                    row.spawn((
                        Text::new(format!("Silver x {}", state.silver)),
                        TextFont {
                            font_size: FontSize::Px(16.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });
            }
            for entry in &state.entries {
                let durability = if entry.max_durability == 0 {
                    String::new()
                } else {
                    format!(
                        " | durability {}/{}",
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
                            icon::spawn(host, icon, liveries.as_deref());
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

    fn app(state: LootState) -> App {
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
                voice_range_blocks: 0.0,
            }))
            .insert_resource(InputMode::Loot)
            .insert_resource(LootWindow::from_server(state))
            .add_plugins(LootUiPlugin);
        app.update();
        app
    }

    #[test]
    fn currency_only_loot_is_visible_without_an_item_entry() {
        let mut app = app(LootState {
            corpse_id: 89,
            revision: 3,
            entries: Vec::new(),
            silver: 23,
        });

        let world = app.world_mut();
        let mut texts = world.query::<&Text>();
        assert!(texts.iter(world).any(|text| text.0 == "Silver x 23"));
        let mut silver = world.query_filtered::<Entity, With<LootSilver>>();
        assert_eq!(silver.iter(world).count(), 1);
        let mut buttons = world.query_filtered::<Entity, With<LootEntryButton>>();
        assert_eq!(buttons.iter(world).count(), 0);
    }

    #[test]
    fn the_window_draws_complete_entries_and_a_click_only_emits_the_entry_id() {
        let mut app = app(LootState {
            corpse_id: 90,
            revision: 4,
            entries: vec![LootEntry {
                entry_id: 44,
                item_id: 1,
                count: 3,
                durability: 7,
                max_durability: 10,
            }],
            silver: 17,
        });

        let world = app.world_mut();
        let mut roots = world.query_filtered::<&Visibility, With<LootRoot>>();
        assert_eq!(roots.single(world).unwrap(), &Visibility::Visible);
        let mut texts = world.query::<&Text>();
        let lines: Vec<_> = texts.iter(world).map(|text| text.0.as_str()).collect();
        assert!(lines.contains(&"Loot | revision 4"));
        // The take-all line, spelled out rather than matched loosely, because what is
        // under test is partly the *glyphs*: Bevy's embedded fallback font is a 95-glyph
        // ASCII subset, so a hint written with a typographic dash draws as "F  take all"
        // while still passing any `contains("take all")` somebody might write instead.
        let hint = lines
            .iter()
            .find(|line| line.contains("take all"))
            .expect("the window names the take-all control");
        assert_eq!(*hint, "F - take all");
        assert!(hint.is_ascii(), "the hint carries an undrawable glyph");
        assert!(lines.contains(&"Silver x 17"));
        assert!(
            lines
                .iter()
                .any(|line| { line.contains("x 3") && line.contains("durability 7/10") })
        );

        let mut silver = world.query_filtered::<Entity, With<LootSilver>>();
        assert_eq!(silver.iter(world).count(), 1);
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
