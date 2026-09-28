//! The station panel and the world prompt that teaches the key opening it.
//!
//! Both are pictures of `player::station`. The prompt says which station the interact key
//! would open this frame; the panel says which station is open and lists exactly the
//! recipes made there. A row is the pack's own row — built by `ui/inventory.rs`, greyed by
//! its `refresh_recipe_rows` and reported by its `craft_clicks` — so a press becomes the same
//! `CraftRequest` and changes nothing locally. What a player may make there is the recipe
//! mirror's; whether it works is the server's.
//! The forge also lists worn inventory slots and a display-only full-repair price.
//! Pressing one emits a slot intent; only the next server inventory changes its row.

use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;

use super::inventory::spawn_recipe_rows_at;
use crate::net::{Session, StructureKind};
use crate::player::{
    ApplyInventory, InputMode, Inventory, OriginateStationRepair, REPAIR_SILVER_PER_POINT,
    StationHint, StationRepairClick, StationWindow, item_label, repair_previews, station_title,
};

const WIDTH: f32 = 430.0;

/// The label the prompt names: the *default* interact binding, for the reason
/// `ui/loot.rs`'s take-all line gives — a hint is presentation, and this module does not
/// read `Settings`.
const USE_KEY: &str = "F";

/// The loot window's hint ink, so the two lines that teach the interact key read alike.
const HINT: Color = Color::srgb(0.62, 0.62, 0.66);

/// How far under the crosshair's centre the prompt sits, in logical pixels: clear of the
/// crosshair and its mining ring, near enough to read as being about what is aimed at.
const PROMPT_OFFSET: f32 = 44.0;

#[derive(Component)]
struct StationRoot;

#[derive(Component)]
struct StationTitle;

#[derive(Component)]
struct StationPrompt;

/// The column the open station's recipe rows are built into, under the title.
#[derive(Component)]
struct StationRecipeList;

#[derive(Component)]
struct StationScroll;

#[derive(Component)]
struct StationRepairList;

#[derive(Component)]
struct RepairButton(u16);

pub(super) struct StationUiPlugin;

impl Plugin for StationUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StationWindow>()
            .init_resource::<StationHint>()
            .init_resource::<InputMode>()
            .init_resource::<Inventory>()
            .add_message::<StationRepairClick>()
            .add_systems(Startup, spawn_station_ui)
            .add_systems(
                Update,
                (
                    rebuild_recipe_rows,
                    show_panel,
                    show_prompt.after(crate::player::ChestPromptUpdate),
                    scroll_station,
                ),
            )
            .add_systems(
                Update,
                (rebuild_repair_rows, repair_clicks)
                    .chain()
                    .in_set(OriginateStationRepair)
                    .after(ApplyInventory),
            );
    }
}

/// Rebuilds the rows when the open station changes, and clears them when it closes.
///
/// Rows are rebuilt rather than filtered because a panel shows one station at a time, and
/// the set of rows that station makes is static for as long as it is open.
fn rebuild_recipe_rows(
    window: Res<StationWindow>,
    lists: Query<Entity, With<StationRecipeList>>,
    mut commands: Commands,
) {
    if !window.is_changed() {
        return;
    }
    for list in &lists {
        commands.entity(list).despawn_related::<Children>();
        if let Some(kind) = window.station() {
            commands
                .entity(list)
                .with_children(|list| spawn_recipe_rows_at(list, Some(kind)));
        }
    }
}

/// Re-render only from the newest complete InventoryState. A click never removes a
/// row or spends its displayed silver; the server answer is the next change signal.
fn rebuild_repair_rows(
    window: Res<StationWindow>,
    inventory: Res<Inventory>,
    mut lists: Query<(Entity, &mut Node), With<StationRepairList>>,
    mut commands: Commands,
) {
    if !window.is_changed() && !inventory.is_changed() {
        return;
    }
    for (list, mut node) in &mut lists {
        commands.entity(list).despawn_related::<Children>();
        let forge = window.station() == Some(StructureKind::Forge);
        node.display = if forge { Display::Flex } else { Display::None };
        if !forge {
            continue;
        }
        let rows = repair_previews(&inventory);
        commands.entity(list).with_children(|list| {
            list.spawn((
                Text::new(format!("Repair  |  {} silver", inventory.silver())),
                TextFont {
                    font_size: FontSize::Px(18.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            list.spawn((
                Text::new(format!(
                    "Full repair. Cost preview: missing points x {REPAIR_SILVER_PER_POINT} silver."
                )),
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(HINT),
            ));
            if rows.is_empty() {
                list.spawn((
                    Text::new("No worn items in your inventory."),
                    TextFont {
                        font_size: FontSize::Px(14.0),
                        ..default()
                    },
                    TextColor(HINT),
                ));
            }
            for row in rows {
                list.spawn((
                    Button,
                    RepairButton(row.slot),
                    Node {
                        flex_direction: FlexDirection::Column,
                        flex_shrink: 0.0,
                        padding: UiRect::all(Val::Px(8.0)),
                        row_gap: Val::Px(3.0),
                        ..default()
                    },
                    BackgroundColor(super::BUTTON),
                ))
                .with_children(|button| {
                    button.spawn((
                        Text::new(format!(
                            "Repair {} - slot {}",
                            item_label(row.item_id),
                            u32::from(row.slot) + 1
                        )),
                        TextFont {
                            font_size: FontSize::Px(15.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                    button.spawn((
                        Text::new(format!(
                            "Missing: {} points    Price: {} silver",
                            row.missing, row.price
                        )),
                        TextFont {
                            font_size: FontSize::Px(13.0),
                            ..default()
                        },
                        TextColor(HINT),
                    ));
                });
            }
        });
    }
}

fn repair_clicks(
    mode: Res<InputMode>,
    window: Res<StationWindow>,
    mut rows: Query<(&Interaction, &RepairButton, &mut BackgroundColor), Changed<Interaction>>,
    mut clicks: MessageWriter<StationRepairClick>,
) {
    if *mode != InputMode::Station || window.station() != Some(StructureKind::Forge) {
        return;
    }
    for (interaction, row, mut colour) in &mut rows {
        colour.0 = match interaction {
            Interaction::Pressed => super::BUTTON_PRESSED,
            Interaction::Hovered => super::BUTTON_HOVERED,
            Interaction::None => super::BUTTON,
        };
        if *interaction == Interaction::Pressed {
            clicks.write(StationRepairClick { slot: row.0 });
        }
    }
}

/// One bounded viewport holds both repairs and recipes, so a full worn pack cannot
/// push crafting off the screen. Scroll any place while this panel owns the mode.
fn scroll_station(
    mode: Res<InputMode>,
    window: Res<StationWindow>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    mut areas: Query<(&mut ScrollPosition, &ComputedNode), With<StationScroll>>,
) {
    if *mode != InputMode::Station {
        return;
    }
    for (mut position, node) in &mut areas {
        let maximum =
            ((node.content_size().y - node.size().y) * node.inverse_scale_factor).max(0.0);
        let delta = scroll.as_ref().map_or(0.0, |scroll| match scroll.unit {
            MouseScrollUnit::Line => -scroll.delta.y * 36.0,
            MouseScrollUnit::Pixel => -scroll.delta.y,
        });
        let next = if window.is_changed() {
            0.0
        } else {
            (position.y + delta).clamp(0.0, maximum)
        };
        if next != position.y {
            position.y = next;
        }
    }
}

fn spawn_station_ui(mut commands: Commands) {
    commands
        .spawn((
            StationRoot,
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
                ..default()
            },
            UiTransform::from_translation(Val2::percent(-50.0, -50.0)),
            // The loot, vendor and prompt frame, exactly.
            BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.96)),
            GlobalZIndex(30),
            Visibility::Hidden,
        ))
        .with_children(|panel| {
            panel.spawn((
                StationTitle,
                Text::new(""),
                TextFont {
                    font_size: FontSize::Px(22.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            panel
                .spawn((
                    StationScroll,
                    ScrollPosition::default(),
                    Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(12.0),
                        min_height: Val::Px(0.0),
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                ))
                .with_children(|content| {
                    content.spawn((
                        StationRepairList,
                        Node {
                            display: Display::None,
                            flex_direction: FlexDirection::Column,
                            flex_shrink: 0.0,
                            row_gap: Val::Px(6.0),
                            ..default()
                        },
                    ));
                    content.spawn((
                        StationRecipeList,
                        Node {
                            flex_direction: FlexDirection::Column,
                            flex_shrink: 0.0,
                            row_gap: Val::Px(6.0),
                            ..default()
                        },
                    ));
                });
            panel.spawn((
                Text::new("Mouse wheel: scroll"),
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(HINT),
            ));
        });

    commands.spawn((
        StationPrompt,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(14.0),
            ..default()
        },
        TextColor(HINT),
        // Over the world rather than on a frame, so it needs the shadow the loot window's
        // dark panel otherwise provides.
        TextShadow::default(),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            ..default()
        },
        UiTransform::from_translation(Val2::new(Val::Percent(-50.0), Val::Px(PROMPT_OFFSET))),
        GlobalZIndex(10),
        Visibility::Hidden,
    ));
}

/// Titles the panel by its station, and shows it while the station mode owns the screen.
fn show_panel(
    window: Res<StationWindow>,
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Visibility, With<StationRoot>>,
    mut titles: Query<&mut Text, With<StationTitle>>,
) {
    let open = window
        .station()
        .filter(|_| session.is_some() && *mode == InputMode::Station);
    if let Some(kind) = open {
        for mut title in &mut titles {
            if title.0 != station_title(kind) {
                title.0 = station_title(kind).to_owned();
            }
        }
    }
    let next = if open.is_some() {
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

/// "F - use forge" under the crosshair while a station would take the key.
///
/// ASCII with a hyphen, for the reason the take-all line gives: Bevy's default font has no
/// em dash, and a hint that draws a gap where its only punctuation should be hides the one
/// thing it is for.
fn show_prompt(
    chest: Option<Res<crate::player::ChestHint>>,
    hint: Res<StationHint>,
    session: Option<Res<Session>>,
    mut prompts: Query<(&mut Text, &mut Visibility), With<StationPrompt>>,
) {
    let shown = session.as_ref().and_then(|_| {
        chest
            .as_deref()
            .and_then(|chest| chest.0.clone())
            .or_else(|| hint.0.map(|kind| prompt_line(station_title(kind))))
    });
    for (mut text, mut visibility) in &mut prompts {
        if let Some(line) = &shown
            && text.0 != *line
        {
            text.0.clone_from(line);
        }
        let next = if shown.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != next {
            *visibility = next;
        }
    }
}

fn prompt_line(title: &str) -> String {
    format!("{USE_KEY} - use {}", title.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::super::inventory::CraftRow;
    use super::*;
    use crate::net::{ANY_TOKEN, SessionParams, StructureKind};

    fn app() -> App {
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
            .add_plugins(StationUiPlugin);
        app.update();
        app
    }

    fn prompt(app: &mut App) -> (String, Visibility) {
        let world = app.world_mut();
        let (text, visibility) = world
            .query_filtered::<(&Text, &Visibility), With<StationPrompt>>()
            .single(world)
            .expect("one station prompt");
        (text.0.clone(), *visibility)
    }

    #[test]
    fn the_prompt_names_the_station_in_ascii_and_hides_without_one() {
        let mut app = app();
        assert_eq!(prompt(&mut app).1, Visibility::Hidden);

        app.insert_resource(StationHint(Some(StructureKind::LeatherBench)));
        app.update();
        let (line, visibility) = prompt(&mut app);
        assert_eq!(line, "F - use leather bench");
        assert!(line.is_ascii(), "the prompt carries an undrawable glyph");
        assert_eq!(visibility, Visibility::Visible);

        app.insert_resource(StationHint(None));
        app.update();
        assert_eq!(prompt(&mut app).1, Visibility::Hidden);
    }

    #[test]
    fn the_panel_is_titled_by_its_station_and_shown_only_in_the_station_mode() {
        let mut app = app();
        app.insert_resource(StationWindow::at(900, StructureKind::ArmourBench));
        app.update();
        let hidden = {
            let world = app.world_mut();
            *world
                .query_filtered::<&Visibility, With<StationRoot>>()
                .single(world)
                .expect("one station panel")
        };
        assert_eq!(
            hidden,
            Visibility::Hidden,
            "a panel opened outside its mode"
        );

        app.insert_resource(InputMode::Station);
        app.update();
        let world = app.world_mut();
        let visibility = *world
            .query_filtered::<&Visibility, With<StationRoot>>()
            .single(world)
            .expect("one station panel");
        assert_eq!(visibility, Visibility::Visible);
        let title = world
            .query_filtered::<&Text, With<StationTitle>>()
            .single(world)
            .expect("one title");
        assert_eq!(title.0, "Armour bench");
    }

    fn panel_rows(app: &mut App) -> Vec<(Entity, crate::net::RecipeId)> {
        let world = app.world_mut();
        world
            .query::<(Entity, &CraftRow)>()
            .iter(world)
            .map(|(entity, row)| (entity, row.0.id))
            .collect()
    }

    fn repair_buttons(app: &mut App) -> Vec<(Entity, u16)> {
        let world = app.world_mut();
        let mut buttons: Vec<_> = world
            .query::<(Entity, &RepairButton)>()
            .iter(world)
            .map(|(entity, button)| (entity, button.0))
            .collect();
        buttons.sort_by_key(|(_, slot)| *slot);
        buttons
    }

    #[test]
    fn forge_repairs_show_all_worn_slots_and_wait_for_the_authoritative_inventory() {
        use crate::net::InventoryStack;
        let mut stacks = vec![InventoryStack::default(); 41];
        stacks[0] = InventoryStack {
            item_id: 10,
            count: 1,
            durability: 99,
            max_durability: 200,
        };
        stacks[40] = InventoryStack {
            item_id: 10,
            count: 1,
            durability: 0,
            max_durability: 200,
        };
        let initial = Inventory::from_state(stacks.clone(), 0);
        let mut app = app();
        app.insert_resource(initial.clone())
            .insert_resource(StationWindow::at(900, StructureKind::Forge))
            .insert_resource(InputMode::Station);
        app.update();
        let rows = repair_buttons(&mut app);
        assert_eq!(
            rows.iter().map(|(_, slot)| *slot).collect::<Vec<_>>(),
            vec![0, 40]
        );
        let before = app.world().resource::<Inventory>().clone();
        *app.world_mut().get_mut::<Interaction>(rows[1].0).unwrap() = Interaction::Pressed;
        app.update();
        let clicks: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<StationRepairClick>>()
            .drain()
            .collect();
        assert_eq!(clicks, vec![StationRepairClick { slot: 40 }]);
        assert_eq!(
            app.world().resource::<Inventory>(),
            &before,
            "a poor purse is only a preview; the client spends nothing"
        );
        assert_eq!(
            repair_buttons(&mut app),
            rows,
            "clicking must not optimistically remove the row"
        );
        app.update();
        assert!(
            app.world_mut()
                .resource_mut::<Messages<StationRepairClick>>()
                .drain()
                .next()
                .is_none(),
            "holding a press repeated the request"
        );

        // InventoryPlugin publishes the complete server answer as this same resource.
        stacks[40].durability = stacks[40].max_durability;
        app.insert_resource(Inventory::from_state(stacks, 57));
        app.update();
        assert_eq!(
            repair_buttons(&mut app)
                .iter()
                .map(|(_, slot)| *slot)
                .collect::<Vec<_>>(),
            vec![0]
        );
        let world = app.world_mut();
        assert!(
            world
                .query::<&Text>()
                .iter(world)
                .any(|text| text.0 == "Repair  |  57 silver")
        );
        assert!(
            world
                .query::<&Text>()
                .iter(world)
                .any(|text| text.0 == "Missing: 101 points    Price: 101 silver")
        );
    }

    #[test]
    fn repair_section_is_absent_at_every_nonforge_station_and_on_close() {
        use crate::net::InventoryStack;
        let mut app = app();
        app.insert_resource(Inventory::from_stacks(vec![InventoryStack {
            item_id: 10,
            count: 1,
            durability: 0,
            max_durability: 200,
        }]))
        .insert_resource(InputMode::Station);
        for kind in [
            StructureKind::Forge,
            StructureKind::LeatherBench,
            StructureKind::ArmourBench,
            StructureKind::Campfire,
            StructureKind::EnchantingTable,
        ] {
            app.insert_resource(StationWindow::at(900, kind));
            app.update();
            assert_eq!(
                repair_buttons(&mut app).len(),
                usize::from(kind == StructureKind::Forge)
            );
            let world = app.world_mut();
            let node = world
                .query_filtered::<&Node, With<StationRepairList>>()
                .single(world)
                .unwrap();
            assert_eq!(
                node.display,
                if kind == StructureKind::Forge {
                    Display::Flex
                } else {
                    Display::None
                }
            );
        }
        app.insert_resource(StationWindow::default());
        app.update();
        assert!(repair_buttons(&mut app).is_empty());
    }

    #[test]
    fn repair_and_recipe_rows_share_a_bounded_scroll_viewport() {
        let mut app = app();
        let world = app.world_mut();
        let (area, node, _) = world
            .query_filtered::<(Entity, &Node, &ScrollPosition), With<StationScroll>>()
            .single(world)
            .unwrap();
        assert_eq!(node.min_height, Val::Px(0.0));
        assert_eq!(node.overflow, Overflow::scroll_y());
        let recipe_parent = world
            .query_filtered::<&ChildOf, With<StationRecipeList>>()
            .single(world)
            .unwrap()
            .parent();
        let repair_parent = world
            .query_filtered::<&ChildOf, With<StationRepairList>>()
            .single(world)
            .unwrap()
            .parent();
        assert_eq!(recipe_parent, area);
        assert_eq!(repair_parent, area);
    }

    /// Exactly the open station's recipes, greyed by what the pack holds, and a press on an
    /// affordable row asks for that craft while a short one asks for nothing.
    ///
    /// The inventory's own `refresh_recipe_rows` and `craft_clicks` run here, because they
    /// are what the game runs for these rows: the panel reuses them rather than copying them.
    #[test]
    fn the_panel_lists_that_stations_recipes_greys_the_short_ones_and_a_press_asks_to_craft() {
        use super::super::BUTTON;
        use super::super::inventory::{RECIPE_ROW_SHORT, craft_clicks, refresh_recipe_rows};
        use crate::net::{InventoryStack, RecipeId};
        use crate::player::{CraftClick, Inventory, recipes_made_at};

        let cap = recipes_made_at(Some(StructureKind::LeatherBench))
            .find(|recipe| recipe.id == RecipeId::LeatherCap)
            .expect("the leather bench makes a cap");
        let pelts = cap.ingredients[0];

        let mut app = app();
        app.add_message::<CraftClick>()
            .insert_resource(Inventory::from_stacks(vec![InventoryStack {
                item_id: pelts.item_id,
                count: pelts.count,
                ..Default::default()
            }]))
            .add_systems(Update, (refresh_recipe_rows, craft_clicks))
            .insert_resource(StationWindow::at(900, StructureKind::LeatherBench))
            .insert_resource(InputMode::Station);
        app.update();
        app.update();

        let rows = panel_rows(&mut app);
        let expected: Vec<RecipeId> = recipes_made_at(Some(StructureKind::LeatherBench))
            .map(|recipe| recipe.id)
            .collect();
        assert_eq!(rows.iter().map(|(_, id)| *id).collect::<Vec<_>>(), expected);

        let row = |id: RecipeId| {
            rows.iter()
                .find(|(_, row)| *row == id)
                .map(|(entity, _)| *entity)
                .unwrap_or_else(|| panic!("{id:?} has a row"))
        };
        let colour =
            |app: &App, id: RecipeId| app.world().get::<BackgroundColor>(row(id)).unwrap().0;
        assert_eq!(colour(&app, RecipeId::LeatherCap), BUTTON);
        assert_eq!(
            colour(&app, RecipeId::LeatherJerkin),
            RECIPE_ROW_SHORT,
            "three pelts drew a five-pelt jerkin as available"
        );

        let press = |app: &mut App, id: RecipeId| -> Vec<CraftClick> {
            *app.world_mut().get_mut::<Interaction>(row(id)).unwrap() = Interaction::Pressed;
            app.update();
            app.world_mut()
                .resource_mut::<Messages<CraftClick>>()
                .drain()
                .collect()
        };
        assert_eq!(
            press(&mut app, RecipeId::LeatherCap),
            vec![CraftClick {
                recipe: RecipeId::LeatherCap
            }]
        );
        assert!(press(&mut app, RecipeId::LeatherJerkin).is_empty());
        assert_eq!(
            app.world().resource::<Inventory>().count(pelts.item_id),
            u32::from(pelts.count),
            "a press spent a material locally"
        );

        // Another station replaces the rows wholesale.
        app.insert_resource(StationWindow::at(901, StructureKind::Forge));
        app.update();
        let forge: Vec<RecipeId> = recipes_made_at(Some(StructureKind::Forge))
            .map(|recipe| recipe.id)
            .collect();
        assert_eq!(
            panel_rows(&mut app)
                .into_iter()
                .map(|(_, id)| id)
                .collect::<Vec<_>>(),
            forge
        );
    }
    #[test]
    fn chest_reuses_the_station_prompt_and_wins_when_both_are_present() {
        let mut app = app();
        app.insert_resource(StationHint(Some(StructureKind::LeatherBench)));
        app.insert_resource(crate::player::ChestHint(Some("F: Chest".to_owned())));
        app.update();
        let world = app.world_mut();
        let mut prompts = world.query_filtered::<(&Text, &Visibility), With<StationPrompt>>();
        let (text, visibility) = prompts.single(world).unwrap();
        assert_eq!(text.0, "F: Chest");
        assert_eq!(*visibility, Visibility::Visible);
    }
}
