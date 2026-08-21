//! The full authoritative inventory, the recipes read beside it, and the click intents
//! both originate.
//!
//! Two panels on one screen, and neither of them changes a count. The cells are the last
//! complete `InventoryState` the server sent; the recipe rows are the display-only mirror
//! in [`crate::player::RECIPES`], which spells out what a craft costs so that crafting is
//! a plan instead of a guess. A row that is short of materials draws disabled — a courtesy
//! read from [`Inventory::count`] — and a row whose recipe needs a forge says so and stays
//! clickable, because how close the player is standing to one is the server's answer and
//! not this client's.
//!
//! Hovering a filled cell names what is in it. That is one reader of the one display
//! registry in [`crate::player`]; the picture drawn in the cell under the pointer is
//! another, and the hand is the third. A tooltip originates nothing: no message, no
//! request, no count.
//!
//! A cell draws its item rather than its colour — see [`super::icon`] — so an item is
//! readable at a glance instead of on hover, and two items that share a palette entry stop
//! being the same square.

use bevy::prelude::*;
use bevy::ui::FocusPolicy;
use bevy::window::PrimaryWindow;

use super::icon::DrawnIcon;
use super::{
    CELL_EDGE, SELECTED_EDGE, SlotCount, cell_node, refresh_cell_contents, spawn_cell_contents,
    stack_style,
};
use crate::net::{Session, StructureKind};
use crate::player::{
    ApplyInventory, CraftClick, Ingredient, InputMode, Inventory, InventoryClick,
    InventoryClickKind, PickedStack, RECIPES, Recipe, item_label,
};

pub(super) struct InventoryUiPlugin;

impl Plugin for InventoryUiPlugin {
    fn build(&self, app: &mut App) {
        // The player plugin registers this too; doing it here as well keeps this module
        // headlessly testable on its own. `add_message` is idempotent.
        app.add_message::<CraftClick>()
            .add_systems(Startup, spawn_inventory_screen)
            .add_systems(
                Update,
                (
                    build_inventory_cells,
                    ApplyDeferred,
                    refresh_inventory_cells,
                    refresh_recipe_rows,
                    show_inventory,
                    hover_tooltip,
                    inventory_clicks,
                    craft_clicks,
                )
                    .chain()
                    .after(ApplyInventory),
            );
    }
}

#[derive(Component)]
struct InventoryRoot;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum InventoryGrid {
    Pack,
    Hotbar,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct InventoryCell {
    slot: u8,
    grid: InventoryGrid,
}

/// The one tooltip node, spawned once and moved rather than respawned.
///
/// One entity is what makes *moving between two filled slots replaces rather than
/// accumulates* structural instead of a rule somebody has to remember: there is nothing to
/// accumulate. It is a child of the overlay, so closing the screen hides it with everything
/// else and no system has to think about that case.
#[derive(Component)]
struct SlotTooltip;

/// One recipe row, carrying the whole mirrored recipe it draws.
///
/// The row holds the [`Recipe`] itself rather than an id to look up, because the mirror is
/// static: there is no state to go stale between the row being built and being read, and
/// the only thing that ever leaves this client is [`Recipe::id`].
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct CraftRow(Recipe);

/// The heading of one recipe row. It carries the recipe so the dimmed state is read from
/// the same value the row itself is, with no walk up the hierarchy to get there.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct CraftTitle(Recipe);

/// One ingredient's `held/needed` label inside a recipe row.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct CraftCost(Ingredient);

/// A recipe whose materials this client can already see, and how it answers the pointer.
/// The three shades are the pause menu's, so a clickable thing looks the same everywhere.
const RECIPE_ROW: Color = Color::srgb(0.16, 0.18, 0.22);
const RECIPE_ROW_HOVERED: Color = Color::srgb(0.25, 0.29, 0.35);
const RECIPE_ROW_PRESSED: Color = Color::srgb(0.42, 0.31, 0.15);

/// A recipe whose materials are short. Flat and unlit by hover, so the row reads as inert
/// rather than as one that did not respond.
const RECIPE_ROW_SHORT: Color = Color::srgb(0.085, 0.095, 0.115);

const RECIPE_TITLE: Color = Color::WHITE;
const RECIPE_TITLE_SHORT: Color = Color::srgb(0.50, 0.53, 0.58);

/// An ingredient the pack already holds enough of, and one it does not. The second is what
/// answers "what am I missing" at a glance, per ingredient rather than per row.
const RECIPE_COST: Color = Color::srgb(0.72, 0.75, 0.80);
const RECIPE_COST_SHORT: Color = Color::srgb(0.88, 0.44, 0.38);

/// The station note. Amber, and never a disabled state: proximity is the server's call.
const RECIPE_STATION: Color = Color::srgb(0.95, 0.76, 0.35);

/// The tooltip's surface and text. Darker than the panel it floats over, inside the same
/// grey the cells are bordered with, so it reads as a label on top rather than a third
/// panel.
const TOOLTIP_BACKGROUND: Color = Color::srgba(0.020, 0.026, 0.036, 0.97);
const TOOLTIP_TEXT: Color = Color::srgb(0.92, 0.94, 0.97);

/// How far from the pointer the tooltip sits, in logical pixels. Enough that the cursor
/// glyph never covers the first letter.
const TOOLTIP_GAP: f32 = 14.0;

fn spawn_inventory_screen(mut commands: Commands) {
    commands
        .spawn((
            InventoryRoot,
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
            GlobalZIndex(30),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(10.0),
                        padding: UiRect::all(Val::Px(24.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.075, 0.085, 0.105)),
                ))
                .with_children(|panel| {
                    panel.spawn(section_title("PACK"));
                    panel.spawn((InventoryGrid::Pack, grid_node()));
                    panel.spawn(section_title("HOTBAR"));
                    panel.spawn((InventoryGrid::Hotbar, grid_node()));
                    panel.spawn(section_title("CRAFTING"));
                    spawn_recipe_rows(panel);
                    panel.spawn((
                        Text::new(
                            "Left click: move stack    Right click: split    \
                             Click a recipe to craft    E: close",
                        ),
                        TextFont {
                            font_size: FontSize::Px(15.0),
                            ..default()
                        },
                        TextColor(Color::srgb(0.72, 0.75, 0.80)),
                    ));
                });
            overlay.spawn(tooltip_bundle());
        });
}

/// The single tooltip node: absolutely positioned, empty, and hidden until a cell is
/// hovered. [`hover_tooltip`] owns its text, its visibility and where it sits.
fn tooltip_bundle() -> impl Bundle {
    (
        SlotTooltip,
        Node {
            position_type: PositionType::Absolute,
            padding: UiRect::axes(Val::Px(9.0), Val::Px(5.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(TOOLTIP_BACKGROUND),
        BorderColor::all(CELL_EDGE),
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(16.0),
            ..default()
        },
        TextColor(TOOLTIP_TEXT),
        TextShadow::default(),
        Visibility::Hidden,
        // Above the panel it floats over, which is the whole point of a tooltip.
        GlobalZIndex(31),
        // And therefore above the cells, which is a trap without this: a node with no
        // `FocusPolicy` *blocks*, so a tooltip the pointer ever landed inside would
        // capture the interaction, the cell under it would go to `Interaction::None`, and
        // the tooltip would hide and reappear every other frame. `TOOLTIP_GAP` keeps the
        // pointer outside it today; `Pass` is what stops that being load-bearing.
        FocusPolicy::Pass,
    )
}

/// Builds one row per mirrored recipe, once.
///
/// The rows are static because the mirror is: one row per recipe, in the order the table
/// declares them, with no session parameter to size them against and nothing to rebuild
/// when a server state arrives. Only the `held/needed` labels and the enabled colours
/// change, and [`refresh_recipe_rows`] owns both.
fn spawn_recipe_rows(panel: &mut ChildSpawnerCommands<'_>) {
    for recipe in RECIPES {
        panel
            .spawn((
                CraftRow(recipe),
                Button,
                Node {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(3.0),
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(7.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(RECIPE_ROW),
            ))
            .with_children(|row| {
                row.spawn((
                    CraftTitle(recipe),
                    Text::new(recipe_heading(&recipe)),
                    TextFont {
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(RECIPE_TITLE),
                    TextShadow::default(),
                ));
                row.spawn(Node {
                    display: Display::Flex,
                    column_gap: Val::Px(14.0),
                    ..default()
                })
                .with_children(|costs| {
                    for ingredient in recipe.ingredients {
                        costs.spawn((
                            CraftCost(*ingredient),
                            Text::new(""),
                            TextFont {
                                font_size: FontSize::Px(15.0),
                                ..default()
                            },
                            TextColor(RECIPE_COST),
                        ));
                    }
                });
                if let Some(station) = recipe.station {
                    row.spawn((
                        Text::new(station_note(station)),
                        TextFont {
                            font_size: FontSize::Px(14.0),
                            ..default()
                        },
                        TextColor(RECIPE_STATION),
                    ));
                }
            });
    }
}

/// The row's heading: what the recipe makes, in the case the section titles beside it use.
fn recipe_heading(recipe: &Recipe) -> String {
    let name = item_label(recipe.product.item_id).to_uppercase();
    if recipe.product.count == 1 {
        name
    } else {
        format!("{name} x{}", recipe.product.count)
    }
}

/// What a recipe needing a station says, and it stays a label rather than becoming a gate.
///
/// The row remains clickable: the structures a snapshot names are the ones in view, so a
/// client that refused to ask without one would refuse crafts the server would have
/// granted. A craft made too far from a forge is refused there, in silence.
fn station_note(station: StructureKind) -> String {
    let name = match station {
        StructureKind::Forge => "forge",
        StructureKind::Tent => "tent",
        StructureKind::Campfire => "campfire",
    };
    format!("requires a {name} nearby")
}

fn section_title(label: &str) -> impl Bundle {
    (
        Text::new(label),
        TextFont {
            font_size: FontSize::Px(22.0),
            ..default()
        },
        TextColor(Color::WHITE),
        TextShadow::default(),
    )
}

fn grid_node() -> Node {
    Node {
        display: Display::Grid,
        row_gap: Val::Px(6.0),
        column_gap: Val::Px(6.0),
        ..default()
    }
}

fn build_inventory_cells(
    mut commands: Commands,
    session: Option<Res<Session>>,
    mut grids: Query<(Entity, &InventoryGrid, &mut Node)>,
    cells: Query<(Entity, &InventoryCell)>,
) {
    let session_changed = session.as_ref().is_some_and(|session| session.is_changed());
    let Some(session) = session else {
        return;
    };
    let expected = usize::from(session.0.inventory_slots);
    // The same total can have a different hotbar/pack split in a later session.
    // A newly inserted Session therefore invalidates the layout even when the
    // number of existing cells still matches.
    if !session_changed && cells.iter().count() == expected {
        return;
    }

    for (entity, _) in &cells {
        commands.entity(entity).despawn();
    }

    for (grid_entity, grid, mut node) in &mut grids {
        node.grid_template_columns =
            RepeatedGridTrack::flex(u16::from(session.0.hotbar_slots), 1.0);
        let range = match grid {
            InventoryGrid::Pack => session.0.hotbar_slots..session.0.inventory_slots,
            InventoryGrid::Hotbar => 0..session.0.hotbar_slots,
        };
        for slot in range {
            commands
                .spawn((
                    InventoryCell { slot, grid: *grid },
                    ChildOf(grid_entity),
                    Button,
                    cell_node(),
                    BackgroundColor(super::EMPTY_CELL),
                    BorderColor::all(CELL_EDGE),
                ))
                // The picture and the count are the same two children the hotbar's cells
                // get, spawned by the same function: one cell, drawn in two places.
                .with_children(spawn_cell_contents);
        }
    }
}

/// Redraws every pack and hotbar cell against the newest authoritative slot.
///
/// The plate and the picked-up edge belong to this screen; what goes *inside* the cell is
/// [`refresh_cell_contents`], shared with the always-visible hotbar so one stack cannot be
/// drawn two ways. `Without<SlotCount>` on the cells is what keeps the two
/// `BackgroundColor` queries disjoint — the cell's plate and the count's plate are the same
/// component on two entities.
fn refresh_inventory_cells(
    mut commands: Commands,
    inventory: Option<Res<Inventory>>,
    picked: Option<Res<PickedStack>>,
    mut cells: Query<
        (
            &InventoryCell,
            &Children,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        Without<SlotCount>,
    >,
    mut counts: Query<(&mut Text, &mut BackgroundColor), With<SlotCount>>,
    mut icons: Query<&mut DrawnIcon>,
) {
    let (Some(inventory), Some(picked)) = (inventory, picked) else {
        return;
    };

    for (cell, children, mut background, mut border) in &mut cells {
        let style = stack_style(inventory.slot(cell.slot));
        if background.0 != style.background {
            background.0 = style.background;
        }
        let edge = if picked.slot() == Some(cell.slot) {
            SELECTED_EDGE
        } else {
            CELL_EDGE
        };
        let next = BorderColor::all(edge);
        if *border != next {
            *border = next;
        }
        refresh_cell_contents(&mut commands, children, &style, &mut counts, &mut icons);
    }
}

/// Redraws every recipe row against the newest complete server state.
///
/// Two things change and nothing else: each ingredient's `held/needed` label, and whether
/// the row reads as available. Both come from [`Recipe::affordable`] and
/// [`Inventory::count`] — the same predicate the sender in `player::crafting` re-reads
/// before a request leaves, which is what makes the drawn state and the sent state agree
/// by construction rather than by two places remembering the same rule.
///
/// **A station is never part of that answer.** A forge recipe with the materials in hand
/// draws exactly like a station-less one and says what it needs in its own note, which is
/// why that note keeps its colour here while the heading dims.
fn refresh_recipe_rows(
    inventory: Option<Res<Inventory>>,
    mut rows: Query<(&CraftRow, &Interaction, &mut BackgroundColor)>,
    mut titles: Query<(&CraftTitle, &mut TextColor), Without<CraftCost>>,
    mut costs: Query<(&CraftCost, &mut Text, &mut TextColor), Without<CraftTitle>>,
) {
    let Some(inventory) = inventory else {
        return;
    };

    for (row, interaction, mut background) in &mut rows {
        let next = match (row.0.affordable(&inventory), interaction) {
            // Short rows ignore hover as well as the press: a row that cannot be used
            // should read as inert rather than as one that failed to respond.
            (false, _) => RECIPE_ROW_SHORT,
            (true, Interaction::Pressed) => RECIPE_ROW_PRESSED,
            (true, Interaction::Hovered) => RECIPE_ROW_HOVERED,
            (true, Interaction::None) => RECIPE_ROW,
        };
        if background.0 != next {
            background.0 = next;
        }
    }

    for (title, mut colour) in &mut titles {
        let next = TextColor(if title.0.affordable(&inventory) {
            RECIPE_TITLE
        } else {
            RECIPE_TITLE_SHORT
        });
        if *colour != next {
            *colour = next;
        }
    }

    for (cost, mut text, mut colour) in &mut costs {
        let held = inventory.count(cost.0.item_id);
        let needed = u32::from(cost.0.count);
        let label = format!("{} {held}/{needed}", item_label(cost.0.item_id));
        if text.0 != label {
            text.0 = label;
        }
        // Per ingredient rather than per row, because "what am I missing" is a question
        // about one line of the cost and a whole-row colour cannot answer it.
        let next = TextColor(if held >= needed {
            RECIPE_COST
        } else {
            RECIPE_COST_SHORT
        });
        if *colour != next {
            *colour = next;
        }
    }
}

fn show_inventory(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Visibility, With<InventoryRoot>>,
) {
    let next = if *mode == InputMode::Inventory && session.is_some() {
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

/// Where the absolutely positioned tooltip is pinned, for one pointer position.
///
/// Two of the four are `Auto`: an absolutely positioned node is anchored by the edges that
/// are not.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TooltipAnchor {
    left: Val,
    right: Val,
    top: Val,
    bottom: Val,
}

/// Anchors the tooltip to the pointer, away from whichever window edge is nearer.
///
/// **Anchored rather than clamped, because the width is not known here.** A node's size is
/// decided by layout, one frame after this runs, so a clamp against the right edge would
/// have to guess how wide the word is and would clip whenever it guessed low. Pinning the
/// *right* edge of the tooltip instead makes it grow leftwards, away from the edge it is
/// near, and the same argument in the other axis keeps it off the bottom of the window. No
/// measurement, and no way to be clipped.
fn anchor_for(cursor: Vec2, window: Vec2) -> TooltipAnchor {
    let (left, right) = if cursor.x * 2.0 <= window.x {
        (Val::Px(cursor.x + TOOLTIP_GAP), Val::Auto)
    } else {
        (
            Val::Auto,
            Val::Px((window.x - cursor.x).max(0.0) + TOOLTIP_GAP),
        )
    };
    let (top, bottom) = if cursor.y * 2.0 <= window.y {
        (Val::Px(cursor.y + TOOLTIP_GAP), Val::Auto)
    } else {
        (
            Val::Auto,
            Val::Px((window.y - cursor.y).max(0.0) + TOOLTIP_GAP),
        )
    };
    TooltipAnchor {
        left,
        right,
        top,
        bottom,
    }
}

/// Names the item under the pointer, and nothing else.
///
/// **Display only, and structurally so.** It writes no message, touches no resource and
/// reads the same [`Interaction`] the recipe rows already answer to, so hovering cannot
/// become a request the way a click can. The name comes from `player`'s display registry —
/// the same table the swatch beside it is coloured from — which is why an item nobody
/// crafts still has one.
///
/// An empty slot shows nothing rather than an empty box: [`Inventory::slot`] answers with
/// the stack the server last sent, and a stack of nothing is not a thing to name.
fn hover_tooltip(
    mode: Res<InputMode>,
    inventory: Option<Res<Inventory>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cells: Query<(&Interaction, &InventoryCell)>,
    mut tooltips: Query<(&mut Node, &mut Text, &mut Visibility), With<SlotTooltip>>,
) {
    let hovered = inventory
        .filter(|_| *mode == InputMode::Inventory)
        .and_then(|inventory: Res<'_, Inventory>| {
            cells
                .iter()
                // The same "any interaction at all" the click reader uses, so a cell held
                // down keeps the label it was hovered with.
                .find(|(interaction, _)| **interaction != Interaction::None)
                .and_then(|(_, cell)| inventory.slot(cell.slot))
                .filter(|stack| stack.item_id != 0 && stack.count != 0)
                .map(|stack| stack.item_id)
        });

    // `None` while the pointer is outside the window, and no window at all in a headless
    // test: in both the tooltip keeps the position it had, because there is nothing newer
    // to move it to.
    let pointer = windows.iter().next().and_then(|window| {
        window
            .cursor_position()
            .map(|cursor| (cursor, Vec2::new(window.width(), window.height())))
    });

    for (mut node, mut text, mut visibility) in &mut tooltips {
        let next = match hovered {
            // `Inherited` rather than `Visible`: the overlay above it owns whether the
            // screen is on at all, and a tooltip must not survive it being closed.
            Some(_) => Visibility::Inherited,
            None => Visibility::Hidden,
        };
        if *visibility != next {
            *visibility = next;
        }

        let label = hovered.map_or("", item_label);
        if text.0 != label {
            text.0 = label.to_owned();
        }

        let Some((cursor, window)) = pointer.filter(|_| hovered.is_some()) else {
            continue;
        };
        let anchor = anchor_for(cursor, window);
        if node.left != anchor.left
            || node.right != anchor.right
            || node.top != anchor.top
            || node.bottom != anchor.bottom
        {
            node.left = anchor.left;
            node.right = anchor.right;
            node.top = anchor.top;
            node.bottom = anchor.bottom;
        }
    }
}

fn inventory_clicks(
    mode: Res<InputMode>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    cells: Query<(&Interaction, &InventoryCell)>,
    mut clicks: MessageWriter<InventoryClick>,
) {
    if *mode != InputMode::Inventory {
        return;
    }
    let Some(buttons) = buttons else {
        return;
    };
    let kind = if buttons.just_pressed(MouseButton::Right) {
        Some(InventoryClickKind::Split)
    } else if buttons.just_pressed(MouseButton::Left) {
        Some(InventoryClickKind::Full)
    } else {
        None
    };
    let Some(kind) = kind else {
        return;
    };

    if let Some((_, cell)) = cells
        .iter()
        .find(|(interaction, _)| **interaction != Interaction::None)
    {
        clicks.write(InventoryClick {
            slot: cell.slot,
            kind,
        });
    }
}

/// Reports one activated recipe row per press, while the screen is open.
///
/// `Changed<Interaction>` with `Pressed`, which is the edge the pause menu already uses: a
/// held button is one activation, not one per frame. The row's own disabled state is the
/// gate — the row that draws short is the row that reports nothing — and the sender in
/// `player::crafting` re-reads the same predicate against the newest state before a frame
/// leaves, because a message written last frame is not a promise about this one.
///
/// **Nothing is decided here.** A reported row becomes a `CraftRequest` and then silence
/// or a complete `InventoryState`; no material is spent and no product appears on this
/// side either way.
fn craft_clicks(
    mode: Res<InputMode>,
    inventory: Option<Res<Inventory>>,
    rows: Query<(&Interaction, &CraftRow), Changed<Interaction>>,
    mut clicks: MessageWriter<CraftClick>,
) {
    if *mode != InputMode::Inventory {
        return;
    }
    let Some(inventory) = inventory else {
        return;
    };

    for (interaction, row) in &rows {
        if *interaction != Interaction::Pressed || !row.0.affordable(&inventory) {
            continue;
        }
        clicks.write(CraftClick { recipe: row.0.id });
    }
}

#[cfg(test)]
mod tests {
    use super::super::{COUNT_PLATE, DrawnCell, FILLED_CELL, drawn_cell, icon};
    use super::*;
    use crate::net::{InventoryStack, RecipeId, SessionParams};
    use crate::player::{ItemShape, SelectedSlot};

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 6,
            hotbar_slots: 2,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn inventory() -> Inventory {
        Inventory::from_stacks(vec![
            InventoryStack {
                item_id: 1,
                count: 5,
                ..Default::default()
            },
            InventoryStack {
                item_id: 0,
                count: 0,
                ..Default::default()
            },
            InventoryStack {
                item_id: u16::MAX,
                count: 2,
                ..Default::default()
            },
            InventoryStack {
                item_id: 0,
                count: 0,
                ..Default::default()
            },
            InventoryStack {
                item_id: 0,
                count: 0,
                ..Default::default()
            },
            InventoryStack {
                item_id: 0,
                count: 0,
                ..Default::default()
            },
        ])
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<InventoryClick>()
            .insert_resource(session())
            .insert_resource(inventory())
            .insert_resource(SelectedSlot::default())
            .init_resource::<PickedStack>()
            .insert_resource(InputMode::Inventory)
            .add_plugins(InventoryUiPlugin);
        app
    }

    #[test]
    fn every_inventory_slot_is_drawn_with_the_hotbar_as_its_own_row() {
        let mut app = app();
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<&InventoryCell>();
        let cells: Vec<InventoryCell> = query.iter(world).copied().collect();
        assert_eq!(cells.len(), 6);
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Hotbar)
                .count(),
            2
        );
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Pack)
                .count(),
            4
        );
    }

    #[test]
    fn a_new_session_rebuilds_the_hotbar_split_when_the_total_is_unchanged() {
        let mut app = app();
        app.update();

        let mut params = session().0;
        params.hotbar_slots = 4;
        app.insert_resource(Session(params));
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<&InventoryCell>();
        let cells: Vec<InventoryCell> = query.iter(world).copied().collect();
        assert_eq!(cells.len(), 6);
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Hotbar)
                .count(),
            4
        );
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Pack)
                .count(),
            2
        );
    }

    #[test]
    fn a_right_click_reports_one_split_intent_for_the_hovered_slot() {
        let mut app = app();
        app.insert_resource(ButtonInput::<MouseButton>::default());
        app.update();

        let cell = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &InventoryCell)>();
            query
                .iter(world)
                .find(|(_, cell)| cell.slot == 2)
                .map(|(entity, _)| entity)
                .expect("slot 2 exists")
        };
        *app.world_mut()
            .entity_mut(cell)
            .get_mut::<Interaction>()
            .expect("buttons carry Interaction") = Interaction::Hovered;
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        app.update();

        let clicks: Vec<InventoryClick> = app
            .world_mut()
            .resource_mut::<Messages<InventoryClick>>()
            .drain()
            .collect();
        assert_eq!(
            clicks,
            vec![InventoryClick {
                slot: 2,
                kind: InventoryClickKind::Split,
            }]
        );
    }

    // -----------------------------------------------------------------------
    // The crafting panel
    // -----------------------------------------------------------------------

    /// The mirrored ids the tests below stock a pack with, named rather than spelled.
    const STONE: u16 = 1;
    const LOG: u16 = 4;
    const COAL: u16 = 5;

    /// Replaces the whole inventory, exactly as an authoritative state does.
    fn deliver(app: &mut App, stacks: &[(u16, u16)]) {
        let stacks = stacks
            .iter()
            .map(|&(item_id, count)| InventoryStack {
                item_id,
                count,
                ..Default::default()
            })
            .collect();
        app.insert_resource(Inventory::from_stacks(stacks));
        app.update();
    }

    fn row_of(app: &mut App, recipe: RecipeId) -> Entity {
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &CraftRow)>();
        query
            .iter(world)
            .find(|(_, row)| row.0.id == recipe)
            .map(|(entity, _)| entity)
            .unwrap_or_else(|| panic!("{recipe:?} has a row"))
    }

    fn row_colour(app: &mut App, recipe: RecipeId) -> Color {
        let row = row_of(app, recipe);
        app.world()
            .get::<BackgroundColor>(row)
            .expect("a row has a background")
            .0
    }

    fn cost_labels(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        let mut query = world.query::<(&CraftCost, &Text)>();
        query.iter(world).map(|(_, text)| text.0.clone()).collect()
    }

    fn press(app: &mut App, recipe: RecipeId) -> Vec<CraftClick> {
        let row = row_of(app, recipe);
        *app.world_mut()
            .entity_mut(row)
            .get_mut::<Interaction>()
            .expect("a button carries an interaction") = Interaction::Pressed;
        app.update();
        app.world_mut()
            .resource_mut::<Messages<CraftClick>>()
            .drain()
            .collect()
    }

    /// One row per mirrored recipe, each headed by what it makes.
    ///
    /// Swept over [`RECIPES`] rather than over a count and four names typed here. The
    /// mirror gained two rows in #113 and this panel needed no edit to draw them, which is
    /// the property worth pinning: a recipe reaches the screen by being in the mirror, and
    /// the mirror is swept against the contract in `player::crafting`'s own tests.
    #[test]
    fn the_panel_lists_every_mirrored_recipe_with_its_cost_and_product() {
        let mut app = app();
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<&CraftRow>();
        let rows: Vec<RecipeId> = query.iter(world).map(|row| row.0.id).collect();
        assert_eq!(
            rows.len(),
            RECIPES.len(),
            "the panel drew a different number of rows than the mirror holds"
        );
        for recipe in RECIPES {
            assert!(rows.contains(&recipe.id), "{:?} has no row", recipe.id);
        }

        // Each row is headed by what it makes, which is the product half of the mirror.
        // Read through the display registry rather than through `recipe_heading`, so this
        // is the panel answering to the item table and not to the function that filled it.
        // The count suffix that function appends is its own business, which is why this
        // asks how the heading starts.
        let mut titles = world.query::<(&CraftTitle, &Text)>();
        let headings: Vec<String> = titles.iter(world).map(|(_, text)| text.0.clone()).collect();
        for recipe in RECIPES {
            let product = item_label(recipe.product.item_id).to_uppercase();
            assert!(
                headings.iter().any(|heading| heading.starts_with(&product)),
                "no row is headed {product}: {headings:?}"
            );
        }
    }

    #[test]
    fn a_cost_line_names_the_ingredient_and_what_is_held_of_it() {
        let mut app = app();
        deliver(&mut app, &[(LOG, 3)]);

        let labels = cost_labels(&mut app);
        assert!(
            labels.contains(&"log 3/8".to_owned()),
            "the tent's cost line did not report 3 of the 8 logs it wants: {labels:?}"
        );
        assert!(
            labels.contains(&"stone 0/8".to_owned()),
            "the forge's cost line did not report an empty stone count: {labels:?}"
        );
    }

    #[test]
    fn the_disabled_state_flips_exactly_at_the_required_counts() {
        let mut app = app();

        deliver(&mut app, &[(LOG, 7)]);
        assert_eq!(
            row_colour(&mut app, RecipeId::Tent),
            RECIPE_ROW_SHORT,
            "seven logs drew an eight-log recipe as available"
        );

        deliver(&mut app, &[(LOG, 8)]);
        assert_eq!(
            row_colour(&mut app, RecipeId::Tent),
            RECIPE_ROW,
            "the eighth log did not enable the row"
        );

        // Counted across slots, exactly as the server spends across slots.
        deliver(&mut app, &[(LOG, 5), (LOG, 3)]);
        assert_eq!(row_colour(&mut app, RecipeId::Tent), RECIPE_ROW);
    }

    /// Proximity is the server's call, so a forge recipe is never grayed out for want of a
    /// forge — only for want of materials, and it says what else it needs in its own note.
    #[test]
    fn a_forge_recipe_with_the_materials_is_enabled_and_labelled() {
        let mut app = app();
        deliver(&mut app, &[(STONE, 2), (COAL, 1)]);

        assert_eq!(
            row_colour(&mut app, RecipeId::SharpeningStone),
            RECIPE_ROW,
            "a recipe this client cannot know the station for was drawn as unavailable"
        );

        let world = app.world_mut();
        let mut query = world.query::<(&Text, &TextColor)>();
        let notes: Vec<String> = query
            .iter(world)
            .filter(|(_, colour)| colour.0 == RECIPE_STATION)
            .map(|(text, _)| text.0.clone())
            .collect();
        assert_eq!(
            notes,
            vec![
                "requires a forge nearby".to_owned(),
                "requires a forge nearby".to_owned()
            ],
            "exactly the two forge recipes carry the station note"
        );
    }

    #[test]
    fn activating_an_enabled_row_reports_that_recipe_and_a_short_one_reports_nothing() {
        let mut app = app();
        deliver(&mut app, &[(LOG, 8)]);
        assert_eq!(
            press(&mut app, RecipeId::Tent),
            vec![CraftClick {
                recipe: RecipeId::Tent
            }]
        );

        // The row that draws short is the row that reports nothing.
        deliver(&mut app, &[(LOG, 7)]);
        assert!(
            press(&mut app, RecipeId::Tent).is_empty(),
            "a short row was activated"
        );
    }

    #[test]
    fn a_reported_row_changes_no_displayed_count() {
        let mut app = app();
        deliver(&mut app, &[(LOG, 8)]);
        let before = app.world().resource::<Inventory>().clone();

        press(&mut app, RecipeId::Tent);
        for _ in 0..4 {
            app.update();
        }

        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "the panel spent a material or produced an item without the server"
        );
        assert!(
            cost_labels(&mut app).contains(&"log 8/8".to_owned()),
            "the cost line consumed the logs it only reported"
        );
    }

    #[test]
    fn a_row_activated_with_the_screen_closed_reports_nothing() {
        let mut app = app();
        deliver(&mut app, &[(LOG, 8)]);
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        assert!(press(&mut app, RecipeId::Tent).is_empty());
    }

    // -----------------------------------------------------------------------
    // The hover tooltip
    // -----------------------------------------------------------------------

    /// The three ids the recipe mirror never mentions, which is why they had no name
    /// before the display registry and would have read "unknown item" here.
    const DIRT: u16 = 2;
    const SNOW: u16 = 3;
    const RUSTY_SWORD: u16 = 7;

    /// Points the pointer at one slot, or at none of them, and runs a frame.
    fn hover(app: &mut App, slot: Option<u8>) {
        let cells: Vec<(Entity, u8)> = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &InventoryCell)>();
            query
                .iter(world)
                .map(|(entity, cell)| (entity, cell.slot))
                .collect()
        };
        for (entity, cell_slot) in cells {
            let want = if Some(cell_slot) == slot {
                Interaction::Hovered
            } else {
                Interaction::None
            };
            *app.world_mut()
                .entity_mut(entity)
                .get_mut::<Interaction>()
                .expect("a cell is a button") = want;
        }
        app.update();
    }

    /// What the one tooltip says and whether it is on screen.
    ///
    /// `single` is half the assertion: a second tooltip entity fails here rather than
    /// being quietly averaged into a passing test.
    fn tooltip(app: &mut App) -> (String, Visibility) {
        let world = app.world_mut();
        let mut query = world.query_filtered::<(&Text, &Visibility), With<SlotTooltip>>();
        let (text, visibility) = query.single(world).expect("exactly one tooltip");
        (text.0.clone(), *visibility)
    }

    #[test]
    fn hovering_a_filled_slot_names_what_is_in_it() {
        let mut app = app();
        app.update();

        hover(&mut app, Some(0));
        assert_eq!(
            tooltip(&mut app),
            ("stone".to_owned(), Visibility::Inherited)
        );
    }

    #[test]
    fn an_empty_slot_names_nothing_and_leaving_takes_the_tooltip_away() {
        let mut app = app();
        app.update();

        hover(&mut app, Some(1));
        assert_eq!(
            tooltip(&mut app),
            (String::new(), Visibility::Hidden),
            "an empty slot was labelled"
        );

        hover(&mut app, Some(0));
        assert_eq!(tooltip(&mut app).1, Visibility::Inherited);

        hover(&mut app, None);
        assert_eq!(
            tooltip(&mut app),
            (String::new(), Visibility::Hidden),
            "the tooltip outlived the pointer that opened it"
        );
    }

    #[test]
    fn moving_between_two_filled_slots_replaces_the_one_tooltip() {
        let mut app = app();
        deliver(&mut app, &[(STONE, 5), (0, 0), (LOG, 3)]);

        hover(&mut app, Some(0));
        assert_eq!(tooltip(&mut app).0, "stone");

        hover(&mut app, Some(2));
        assert_eq!(tooltip(&mut app).0, "log");

        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<SlotTooltip>>();
        assert_eq!(
            query.iter(world).count(),
            1,
            "the tooltips accumulated instead of the one moving"
        );
    }

    /// The gap this issue exists for, read through the surface that exposes it.
    ///
    /// Dirt, snow and the rusty sword appear in no recipe, so the name table written for
    /// the recipe panel never covered them and its own sweep could not see that.
    #[test]
    fn the_items_no_recipe_mentions_are_named_in_a_tooltip_too() {
        let mut app = app();
        deliver(&mut app, &[(DIRT, 1), (SNOW, 1), (RUSTY_SWORD, 1)]);

        for (slot, name) in [(0u8, "dirt"), (1, "snow"), (2, "rusty sword")] {
            hover(&mut app, Some(slot));
            assert_eq!(tooltip(&mut app).0, name, "slot {slot}");
        }
    }

    /// A closed screen has nothing to hover, and the tooltip says so on its own rather
    /// than relying on the overlay above it being hidden.
    #[test]
    fn a_hovered_cell_names_nothing_while_the_screen_is_closed() {
        let mut app = app();
        deliver(&mut app, &[(STONE, 5)]);
        hover(&mut app, Some(0));
        assert_eq!(tooltip(&mut app).1, Visibility::Inherited);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        hover(&mut app, Some(0));
        assert_eq!(tooltip(&mut app), (String::new(), Visibility::Hidden));
    }

    /// A look is not a click: nothing leaves this client and nothing on it changes.
    #[test]
    fn hovering_reports_no_intent_and_changes_no_count() {
        let mut app = app();
        app.insert_resource(ButtonInput::<MouseButton>::default());
        deliver(&mut app, &[(STONE, 5)]);
        let before = app.world().resource::<Inventory>().clone();

        hover(&mut app, Some(0));
        for _ in 0..4 {
            app.update();
        }

        assert!(
            app.world_mut()
                .resource_mut::<Messages<InventoryClick>>()
                .drain()
                .next()
                .is_none(),
            "a hover became a move intent"
        );
        assert!(
            app.world_mut()
                .resource_mut::<Messages<CraftClick>>()
                .drain()
                .next()
                .is_none(),
            "a hover became a craft intent"
        );
        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "a hover changed a count"
        );
    }

    #[test]
    fn the_tooltip_anchors_away_from_the_nearer_window_edge() {
        let window = Vec2::new(800.0, 600.0);

        assert_eq!(
            anchor_for(Vec2::new(10.0, 10.0), window),
            TooltipAnchor {
                left: Val::Px(10.0 + TOOLTIP_GAP),
                right: Val::Auto,
                top: Val::Px(10.0 + TOOLTIP_GAP),
                bottom: Val::Auto,
            }
        );

        // In the far corner every anchor flips, so the box grows back into the window
        // instead of off it — and it does that without anyone measuring how wide it is.
        assert_eq!(
            anchor_for(Vec2::new(790.0, 590.0), window),
            TooltipAnchor {
                left: Val::Auto,
                right: Val::Px(10.0 + TOOLTIP_GAP),
                top: Val::Auto,
                bottom: Val::Px(10.0 + TOOLTIP_GAP),
            }
        );

        // Exactly on the seam belongs to the near half, so there is one rule and no gap
        // between the two branches.
        let centre = anchor_for(window / 2.0, window);
        assert_eq!(centre.left, Val::Px(400.0 + TOOLTIP_GAP));
        assert_eq!(centre.top, Val::Px(300.0 + TOOLTIP_GAP));
    }

    // -----------------------------------------------------------------------
    // The drawn icons
    // -----------------------------------------------------------------------

    /// The two forge products, which share their swatch with an item that is not a
    /// product: the sharpening stone with stone, the tent with snow.
    const TENT: u16 = 9;
    const SHARPENING_STONE: u16 = 11;

    fn cell_at(app: &mut App, slot: u8) -> Entity {
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &InventoryCell)>();
        query
            .iter(world)
            .find(|(_, cell)| cell.slot == slot)
            .map(|(entity, _)| entity)
            .unwrap_or_else(|| panic!("slot {slot} has a cell"))
    }

    /// The children of one node, owned, so the world can be read again inside the loop.
    fn cell_children(app: &App, parent: Entity) -> Vec<Entity> {
        app.world()
            .get::<Children>(parent)
            .map(|children| children.iter().collect())
            .unwrap_or_default()
    }

    fn drawn(app: &mut App, slot: u8) -> DrawnCell {
        let cell = cell_at(app, slot);
        drawn_cell(app.world(), cell)
    }

    #[test]
    fn a_filled_cell_draws_its_item_and_an_empty_one_draws_none() {
        let mut app = app();
        app.update();

        // The fixture: stone, then an empty slot, then an id no build has a row for.
        assert_eq!(
            drawn(&mut app, 0).rectangles.len(),
            icon::parts(ItemShape::Block).len(),
            "the stone slot did not draw a block"
        );
        assert!(
            drawn(&mut app, 1).rectangles.is_empty(),
            "an empty slot drew a picture"
        );
        assert_eq!(
            drawn(&mut app, 2).rectangles.len(),
            icon::parts(ItemShape::Material).len(),
            "an item from a newer contract drew nothing at all"
        );

        // The cell itself is a plate for the picture rather than the item's colour spread
        // flat, and an empty one keeps exactly the treatment it had.
        let cell = cell_at(&mut app, 0);
        assert_eq!(
            app.world().get::<BackgroundColor>(cell).map(|fill| fill.0),
            Some(FILLED_CELL)
        );
        let empty = cell_at(&mut app, 1);
        assert_eq!(
            app.world().get::<BackgroundColor>(empty).map(|fill| fill.0),
            Some(super::super::EMPTY_CELL)
        );
    }

    /// The refresh path runs every frame, so a cell has to *replace* what it drew.
    #[test]
    fn changing_a_slot_replaces_its_picture_rather_than_accumulating_one() {
        let mut app = app();
        deliver(&mut app, &[(STONE, 5)]);
        let block = drawn(&mut app, 0).rectangles;
        assert_eq!(block.len(), icon::parts(ItemShape::Block).len());

        deliver(&mut app, &[(RUSTY_SWORD, 1)]);
        let blade = drawn(&mut app, 0).rectangles;
        assert_eq!(
            blade.len(),
            icon::parts(ItemShape::Blade).len(),
            "the sword's picture was added to the block's instead of replacing it"
        );
        assert_ne!(blade, block, "the cell kept drawing a block");

        // Several frames over an unchanged stack, which is the state a cell spends almost
        // all of its life in.
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(
            drawn(&mut app, 0).rectangles,
            blade,
            "an untouched stack redrew itself into something else"
        );

        deliver(&mut app, &[(0, 0)]);
        assert!(
            drawn(&mut app, 0).rectangles.is_empty(),
            "an emptied slot kept the picture of what used to be in it"
        );
    }

    /// The count is still there, over the picture, at both ends of a stack.
    #[test]
    fn the_count_is_drawn_over_the_picture_at_one_item_and_at_a_full_stack() {
        let mut app = app();
        for count in [1u16, 64, 999] {
            deliver(&mut app, &[(STONE, count)]);
            let cell = drawn(&mut app, 0);
            assert_eq!(cell.count, count.to_string());
            assert_eq!(
                cell.plate, COUNT_PLATE,
                "the count lost the plate that keeps it readable over the icon"
            );
            assert_eq!(
                cell.rectangles.len(),
                icon::parts(ItemShape::Block).len(),
                "the count replaced the picture instead of sitting over it"
            );
        }

        deliver(&mut app, &[(0, 0)]);
        let empty = drawn(&mut app, 0);
        assert_eq!(empty.count, "");
        assert_eq!(
            empty.plate,
            Color::NONE,
            "an empty cell drew a plate for a count it does not have"
        );
    }

    /// The case the feature exists for, at the surface a player reads it on.
    ///
    /// Eight palette entries across eleven items means collisions by construction. Both
    /// pairs below present as one swatch and have to be told apart by their picture — and
    /// every shape happens to use the same number of rectangles, so this compares the
    /// rectangles themselves rather than counting them.
    #[test]
    fn two_items_that_share_a_swatch_draw_different_cells() {
        let swatch = |item_id| {
            super::super::stack_style(Some(InventoryStack {
                item_id,
                count: 1,
                ..Default::default()
            }))
            .icon
            .expect("a known item is drawn")
            .colour
        };

        let mut app = app();
        deliver(
            &mut app,
            &[(STONE, 1), (SHARPENING_STONE, 1), (SNOW, 1), (TENT, 1)],
        );

        for (left, right) in [(0u8, 1u8), (2, 3)] {
            let (one, other) = (drawn(&mut app, left), drawn(&mut app, right));
            assert!(
                !one.rectangles.is_empty() && !other.rectangles.is_empty(),
                "slots {left} and {right} did not both draw something"
            );
            assert_ne!(
                one.rectangles, other.rectangles,
                "slots {left} and {right} share a swatch and drew the same picture"
            );
        }

        // The collisions themselves, named rather than inferred from the cells above: if
        // either pair ever stops sharing a swatch, this test is measuring nothing.
        assert_eq!(swatch(STONE), swatch(SHARPENING_STONE));
        assert_eq!(swatch(SNOW), swatch(TENT));
    }

    /// A part reaches the screen as the rectangle it declares, tilt included.
    ///
    /// The blade is the one shape whose reading depends on it: three axis-aligned bars
    /// would be a cross, not a sword.
    #[test]
    fn a_blade_is_drawn_tilted_and_a_block_is_not() {
        let mut app = app();
        deliver(&mut app, &[(RUSTY_SWORD, 1), (STONE, 1)]);

        let blade = drawn(&mut app, 0).rectangles;
        assert_eq!(blade.len(), icon::parts(ItemShape::Blade).len());
        assert!(
            blade.iter().all(
                |(node, transform, _)| node.position_type == PositionType::Absolute
                    && transform.rotation != Rot2::IDENTITY
            ),
            "the sword drew square-on: {blade:?}"
        );

        assert!(
            drawn(&mut app, 1)
                .rectangles
                .iter()
                .all(|(_, transform, _)| transform.rotation == Rot2::IDENTITY),
            "a block was drawn at an angle"
        );
    }

    /// A picture is drawn *over* its cell, so it must not take the pointer from it.
    ///
    /// Without `FocusPolicy::Pass` on the icon nodes this fails in the worst shape
    /// available: clicking a full slot stops working while clicking an empty one still
    /// does, because only a full one has anything covering it.
    #[test]
    fn a_drawn_cell_still_answers_the_pointer() {
        let mut app = app();
        app.insert_resource(ButtonInput::<MouseButton>::default());
        deliver(&mut app, &[(STONE, 5)]);

        let cell = cell_at(&mut app, 0);
        for child in cell_children(&app, cell) {
            assert_eq!(
                app.world().get::<FocusPolicy>(child),
                Some(&FocusPolicy::Pass),
                "a cell's child blocks the pointer"
            );
            for part in cell_children(&app, child) {
                assert_eq!(
                    app.world().get::<FocusPolicy>(part),
                    Some(&FocusPolicy::Pass),
                    "a drawn rectangle blocks the pointer"
                );
            }
        }

        // And the click the cell still receives is still only a request.
        *app.world_mut()
            .entity_mut(cell)
            .get_mut::<Interaction>()
            .expect("a cell is a button") = Interaction::Hovered;
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        app.update();
        let clicks: Vec<InventoryClick> = app
            .world_mut()
            .resource_mut::<Messages<InventoryClick>>()
            .drain()
            .collect();
        assert_eq!(
            clicks,
            vec![InventoryClick {
                slot: 0,
                kind: InventoryClickKind::Full,
            }]
        );
    }

    /// The live pointer reaches the node, not only the pure helper above.
    #[test]
    fn the_tooltip_is_placed_from_the_primary_window_pointer() {
        let mut app = app();
        let mut window = Window::default();
        // The default 1280x720, with the pointer in the far corner so both anchors flip.
        window.set_cursor_position(Some(Vec2::new(1000.0, 600.0)));
        app.world_mut().spawn((PrimaryWindow, window));

        deliver(&mut app, &[(STONE, 5)]);
        hover(&mut app, Some(0));

        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<SlotTooltip>>();
        let node = query.single(world).expect("exactly one tooltip").clone();
        assert_eq!(node.left, Val::Auto);
        assert_eq!(node.top, Val::Auto);
        assert_eq!(node.right, Val::Px(1280.0 - 1000.0 + TOOLTIP_GAP));
        assert_eq!(node.bottom, Val::Px(720.0 - 600.0 + TOOLTIP_GAP));
    }
}
