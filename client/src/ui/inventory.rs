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

use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::ui::{FocusPolicy, UiSystems};
use bevy::window::{PrimaryWindow, WindowResized};

use super::icon::{self, DrawnIcon};
use super::{
    BUTTON, CELL_EDGE, COUNT_FONT_SIZE, SELECTED_EDGE, SlotCount, TAB_SELECTED, anchor_for,
    button_colour, cell_node, pointer_in_window, refresh_cell_contents, silver_icon,
    spawn_cell_contents, stack_style, tooltip_bundle,
};
#[cfg(test)]
use super::{TOOLTIP_GAP, TooltipAnchor};
use crate::net::{InventoryStack, MountKind, Session, StructureKind};
#[cfg(test)]
use crate::player::EQUIPMENT_ROUTES;
use crate::player::{
    ApplyInventory, CraftClick, Ingredient, InputMode, Inventory, InventoryClick,
    InventoryClickKind, LearnedMounts, Liveries, PickedStack, RECIPES, Recipe, RecipeCategory,
    equipment_item_fits, item_label, mount_label, preference_from_mount,
};
use crate::settings::{Bindings, Control, Settings};

pub(super) struct InventoryUiPlugin;

impl Plugin for InventoryUiPlugin {
    fn build(&self, app: &mut App) {
        // The player plugin registers `CraftClick` and the window plugin registers
        // `WindowResized`; doing both here too keeps this module headlessly testable on
        // its own. `add_message` is idempotent.
        app.add_message::<CraftClick>()
            .add_message::<WindowResized>()
            .init_resource::<InventoryTab>()
            .init_resource::<CraftFilter>()
            .init_resource::<InventoryWindowPosition>()
            .init_resource::<InventoryDrag>()
            .add_systems(Startup, spawn_inventory_screen)
            .add_systems(
                Update,
                (
                    build_inventory_cells,
                    ApplyDeferred,
                    refresh_inventory_cells,
                    refresh_recipe_rows,
                    refresh_silver_readout,
                    show_inventory,
                    // After `show_inventory`, which resets the tab when the screen opens,
                    // so the frame it appears on is already showing the pack.
                    place_inventory_window_on_open,
                    reclamp_inventory_window_on_resize,
                    drag_inventory_window,
                    switch_tabs,
                    show_the_active_tab,
                    switch_craft_filters,
                    show_filtered_recipes,
                    choose_default_mount,
                    rebuild_mount_rows,
                    scroll_crafting,
                    hover_tooltip,
                    inventory_clicks,
                    craft_clicks,
                )
                    .chain()
                    .after(ApplyInventory),
            )
            // `ComputedNode` carries this frame's content and viewport sizes only after
            // layout. Clamping here also covers a resize or filter change with no later
            // wheel event to repair an offset that became too large.
            .add_systems(PostUpdate, sync_craft_scrollbar.after(UiSystems::Layout));
    }
}

#[derive(Component)]
struct InventoryRoot;

/// The inventory's frame, positioned in logical window pixels.
#[derive(Component)]
struct InventoryWindow;

/// The one area from which a drag can begin.
#[derive(Component)]
struct InventoryGrabArea;

/// The strip's identity is useful beyond its buttons: its laid-out position is the
/// stability contract between the two content panels.
#[derive(Component)]
struct InventoryTabStrip;

/// How much silver the last authoritative state put in this player's purse.
///
/// **A child of the window and a sibling of the tab strip, not of either tab panel**, which
/// is what makes it visible on both halves without either half owning it — the same position
/// the strip itself holds, and for the same reason.
///
/// It originates nothing. Silver is not an inventory stack: this reads the dedicated
/// server-owned counter beside the slots and never derives a balance from their contents.
#[derive(Component)]
struct SilverReadout;

/// The number inside the readout, kept off the container so a refresh writes text and
/// nothing else.
#[derive(Component)]
pub(super) struct SilverCount;

/// Where the inventory was last put during this run.
///
/// Deliberately a resource and deliberately absent from `settings`: this is session state,
/// not a preference. `None` means the window has not opened yet and should start centred.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
struct InventoryWindowPosition(Option<Vec2>);

/// The cursor-to-window offset captured at the start of one drag.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
struct InventoryDrag(Option<Vec2>);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum InventoryGrid {
    Pack,
    Hotbar,
    Equipment,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct InventoryCell {
    slot: u8,
    grid: InventoryGrid,
}

/// The body location an empty equipment cell names.
///
/// Kept on the caption child rather than inferred from its text, so refreshing the cell
/// never has to parse presentation back into state. The child is hidden while the slot is
/// full; the same icon and count as every other inventory cell take its place.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct EquipmentCaption(&'static str);

/// The authoritative state needed to redraw a cell, grouped as one system parameter.
///
/// A refresh reads all three as one snapshot: the welcome defines the slot ranges, the
/// inventory supplies their contents, and the picked source decides the selected/refused edge.
#[derive(bevy::ecs::system::SystemParam)]
struct InventoryCellState<'w> {
    session: Option<Res<'w, Session>>,
    inventory: Option<Res<'w, Inventory>>,
    picked: Option<Res<'w, PickedStack>>,
}

/// The one tooltip node, spawned once and moved rather than respawned.
///
/// One entity is what makes *moving between two filled slots replaces rather than
/// accumulates* structural instead of a rule somebody has to remember: there is nothing to
/// accumulate. It is a child of the overlay, so closing the screen hides it with everything
/// else and no system has to think about that case.
#[derive(Component)]
struct SlotTooltip;

/// The filter strip inside the crafting tab.
#[derive(Component)]
struct CraftFilterStrip;

/// One local-only recipe filter.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct CraftFilterButton(CraftFilter);

/// The bounded viewport whose children are the recipe rows.
#[derive(Component)]
struct CraftScrollArea;

/// The visible rail beside the crafting viewport.
#[derive(Component)]
struct CraftScrollbar;

/// The passive thumb that mirrors [`ScrollPosition`]. Scrolling itself is driven by the
/// mouse wheel so no extra Bevy feature or dependency is needed.
#[derive(Component)]
struct CraftScrollbarThumb;

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

/// Which recipe shelf is visible inside the crafting tab.
///
/// `All` preserves the complete mirror and is the default; the other three are purely
/// presentational sub-inventories over the same static rows.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
enum CraftFilter {
    #[default]
    All,
    Survival,
    Tools,
    Armour,
}

impl CraftFilter {
    const ALL: [Self; 4] = [Self::All, Self::Survival, Self::Tools, Self::Armour];

    const fn label(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Survival => "SURVIVAL",
            Self::Tools => "TOOLS",
            Self::Armour => "ARMOUR",
        }
    }

    const fn includes(self, category: RecipeCategory) -> bool {
        match self {
            Self::All => true,
            Self::Survival => matches!(category, RecipeCategory::Survival),
            Self::Tools => matches!(category, RecipeCategory::Tools),
            Self::Armour => matches!(category, RecipeCategory::Armour),
        }
    }
}

/// A recipe whose materials are short. Flat and unlit by hover, so the row reads as inert
/// rather than as one that did not respond.
const RECIPE_ROW_SHORT: Color = Color::srgb(0.085, 0.095, 0.115);

const RECIPE_TITLE: Color = Color::WHITE;
const RECIPE_TITLE_SHORT: Color = Color::srgb(0.50, 0.53, 0.58);

/// An ingredient the pack already holds enough of, and one it does not. The second colour
/// also marks an equipment destination this client's routing table says the picked item does
/// not fit. Both are refusal courtesies, and neither can stop a request leaving the client.
const RECIPE_COST: Color = Color::srgb(0.72, 0.75, 0.80);
const REFUSED_TINT: Color = Color::srgb(0.88, 0.44, 0.38);

/// The station note. Amber, and never a disabled state: proximity is the server's call.
const RECIPE_STATION: Color = Color::srgb(0.95, 0.76, 0.35);

/// A fixed frame is the layout decision that keeps the tab strip stable: tab contents may
/// differ, but neither participates in sizing this node.
const INVENTORY_WINDOW_SIZE: Vec2 = Vec2::new(760.0, 720.0);

/// The space between the ordinary inventory and its one equipment column.
const PACK_EQUIPMENT_GAP: f32 = 18.0;

/// Enough room for the column heading while the cells themselves remain the shared size.
const EQUIPMENT_COLUMN_WIDTH: f32 = 126.0;

/// The current wire order, as `schemas/handshake.fbs` states it. A newer server may announce
/// more equipment cells; those still get drawn and use the neutral fallback below.
const EQUIPMENT_CAPTIONS: [&str; 4] = ["HEAD", "CHEST", "LEGS", "OFF-HAND"];

/// Space between the frame edge and the grab bar. The clamp accounts for it rather than
/// mistaking visible frame padding for visible grab area.
const INVENTORY_WINDOW_PADDING: f32 = 24.0;

/// The grab bar stays this tall regardless of which tab is active.
const GRAB_AREA_HEIGHT: f32 = 34.0;

/// The coin beside the silver count, in logical pixels.
///
/// A third of a cell, so the readout reads as a label rather than as a slot somebody could
/// click. Every part of the picture is a percentage of its host — see `ui::icon::IconPart` —
/// so drawing the same coin small costs this number and nothing else.
const SILVER_ICON_SIZE: f32 = 18.0;

/// The silver count's own ink: brighter than a recipe's cost line, because it is the one
/// number on this screen that is not a comparison.
const SILVER_TEXT: Color = Color::srgb(0.85, 0.88, 0.94);

/// One wheel line in logical pixels. A little under one recipe row, so a single notch
/// advances the list without skipping over a compact recipe entirely.
const CRAFT_SCROLL_LINE: f32 = 48.0;

/// The scrollbar remains grabbable-looking even when the complete list is much longer
/// than its viewport.
const CRAFT_SCROLLBAR_WIDTH: f32 = 9.0;
const CRAFT_SCROLLBAR_MIN_THUMB: f32 = 28.0;

/// Enough horizontal grab bar to recover a window left past either side of the viewport.
const MIN_VISIBLE_GRAB_WIDTH: f32 = 96.0;

/// Which half of the inventory screen is on show.
///
/// **Local presentation and nothing else.** Switching tabs originates no request, reaches no
/// message and tells the server nothing — it is which nodes have a `Display`, decided here.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum InventoryTab {
    /// The pack and the hotbar. What the screen opens on.
    #[default]
    Pack,
    /// The recipe rows, which were a third section of the one panel until #177.
    Crafting,
    /// The authoritative learned horses and the local default selection.
    Mounts,
}

impl InventoryTab {
    /// Every tab, in the order the strip draws them.
    ///
    /// A hand-written list, for the reason `ItemShape::ALL` is one: no stable Rust
    /// enumerates an enum's variants. What keeps it honest is that the strip is built from
    /// it and `label` matches on the variant with no wildcard arm, so a third tab is a
    /// build failure until it has a name — and then it appears in the strip and gets its
    /// own panel with nothing rearranged, which is the acceptance criterion.
    const ALL: [Self; 3] = [Self::Pack, Self::Crafting, Self::Mounts];

    /// What a player reads on the tab.
    const fn label(self) -> &'static str {
        match self {
            Self::Pack => "PACK",
            Self::Crafting => "CRAFTING",
            Self::Mounts => "MOUNTS",
        }
    }
}

/// One tab in the strip: the half it selects.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct TabButton(InventoryTab);

/// The container holding one tab's contents, shown by `Display` and never by `Visibility`.
///
/// `bevy_ui` lays a hidden node out exactly as it lays out a visible one, so a `Visibility`
/// here would leave the crafting rows occupying the panel's height while the pack is up —
/// the same trap `ui/character.rs` records for its two halves, and the reason both use
/// `Display::None`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct TabPanel(InventoryTab);

#[derive(Component)]
struct MountList;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct MountChoice(MountKind);

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
                    InventoryWindow,
                    Node {
                        position_type: PositionType::Absolute,
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        width: Val::Px(INVENTORY_WINDOW_SIZE.x),
                        height: Val::Px(INVENTORY_WINDOW_SIZE.y),
                        flex_shrink: 0.0,
                        row_gap: Val::Px(10.0),
                        padding: UiRect::all(Val::Px(INVENTORY_WINDOW_PADDING)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.075, 0.085, 0.105)),
                ))
                .with_children(|panel| {
                    panel
                        .spawn((
                            InventoryGrabArea,
                            Button,
                            Node {
                                width: Val::Percent(100.0),
                                height: Val::Px(GRAB_AREA_HEIGHT),
                                flex_shrink: 0.0,
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::Center,
                                border_radius: BorderRadius::all(Val::Px(4.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgb(0.10, 0.115, 0.14)),
                        ))
                        .with_child((
                            Text::new("INVENTORY  -  DRAG"),
                            TextFont {
                                font_size: FontSize::Px(15.0),
                                ..default()
                            },
                            TextColor(Color::srgb(0.72, 0.75, 0.80)),
                            FocusPolicy::Pass,
                        ));
                    spawn_tab_strip(panel);

                    panel
                        .spawn(Node {
                            position_type: PositionType::Relative,
                            width: Val::Percent(100.0),
                            flex_grow: 1.0,
                            min_height: Val::Px(0.0),
                            ..default()
                        })
                        .with_children(|content| {
                            // Both halves occupy the same fixed content area. The visible
                            // one is still selected with `Display`; absolute positioning is
                            // what stops its natural height resizing the window around it.
                            content
                                .spawn((
                                    TabPanel(InventoryTab::Pack),
                                    tab_panel_node(Display::Flex),
                                ))
                                .with_children(|pack| {
                                    pack.spawn(pack_and_equipment_node()).with_children(|body| {
                                        body.spawn(pack_column_node()).with_children(|ordinary| {
                                            ordinary.spawn(section_title("PACK"));
                                            ordinary.spawn((InventoryGrid::Pack, grid_node()));
                                            ordinary.spawn(section_title("HOTBAR"));
                                            ordinary.spawn((InventoryGrid::Hotbar, grid_node()));
                                        });
                                        body.spawn(equipment_column_node()).with_children(
                                            |equipment| {
                                                equipment.spawn(section_title("EQUIPMENT"));
                                                equipment
                                                    .spawn((InventoryGrid::Equipment, grid_node()));
                                            },
                                        );
                                    });
                                    pack.spawn(hint(
                                        "Left click: move stack    Right click: split    E: close",
                                    ));
                                });

                            content
                                .spawn((
                                    TabPanel(InventoryTab::Crafting),
                                    tab_panel_node(Display::None),
                                ))
                                .with_children(|crafting| {
                                    spawn_craft_filter_strip(crafting);
                                    crafting
                                        .spawn(Node {
                                            display: Display::Flex,
                                            flex_direction: FlexDirection::Row,
                                            flex_grow: 1.0,
                                            min_height: Val::Px(0.0),
                                            column_gap: Val::Px(8.0),
                                            ..default()
                                        })
                                        .with_children(|body| {
                                            body.spawn((
                                                CraftScrollArea,
                                                Node {
                                                    display: Display::Flex,
                                                    flex_direction: FlexDirection::Column,
                                                    flex_grow: 1.0,
                                                    min_width: Val::Px(0.0),
                                                    min_height: Val::Px(0.0),
                                                    row_gap: Val::Px(10.0),
                                                    padding: UiRect::right(Val::Px(2.0)),
                                                    overflow: Overflow::scroll_y(),
                                                    ..default()
                                                },
                                                ScrollPosition::default(),
                                            ))
                                            .with_children(spawn_recipe_rows);
                                            body.spawn((
                                                CraftScrollbar,
                                                Node {
                                                    position_type: PositionType::Relative,
                                                    width: Val::Px(CRAFT_SCROLLBAR_WIDTH),
                                                    height: Val::Percent(100.0),
                                                    flex_shrink: 0.0,
                                                    border_radius: BorderRadius::all(Val::Px(5.0)),
                                                    ..default()
                                                },
                                                BackgroundColor(Color::srgb(0.045, 0.052, 0.066)),
                                            ))
                                            .with_child((
                                                CraftScrollbarThumb,
                                                Node {
                                                    position_type: PositionType::Absolute,
                                                    left: Val::Px(0.0),
                                                    top: Val::Px(0.0),
                                                    width: Val::Percent(100.0),
                                                    height: Val::Percent(100.0),
                                                    border_radius: BorderRadius::all(Val::Px(5.0)),
                                                    ..default()
                                                },
                                                BackgroundColor(Color::srgb(0.30, 0.33, 0.40)),
                                            ));
                                        });
                                    crafting.spawn(hint("Click a recipe to craft    E: close"));
                                });

                            content
                                .spawn((
                                    TabPanel(InventoryTab::Mounts),
                                    tab_panel_node(Display::None),
                                ))
                                .with_children(|mounts| {
                                    mounts.spawn(section_title("LEARNED MOUNTS"));
                                    mounts.spawn((
                                        MountList,
                                        Node {
                                            display: Display::Flex,
                                            flex_direction: FlexDirection::Column,
                                            flex_grow: 1.0,
                                            row_gap: Val::Px(8.0),
                                            ..default()
                                        },
                                    ));
                                    mounts.spawn(hint(
                                        "Click a learned mount to make it the default    E: close",
                                    ));
                                });
                        });
                    // Last of the window's children, so it draws over whichever tab is
                    // occupying the content area beneath it.
                    spawn_silver_readout(panel, INVENTORY_WINDOW_PADDING);
                });
            overlay.spawn(tooltip_bundle(SlotTooltip));
        });
}

/// Both tab bodies start at the same point and are taken out of their parent's sizing.
fn tab_panel_node(display: Display) -> Node {
    Node {
        position_type: PositionType::Absolute,
        display,
        flex_direction: FlexDirection::Column,
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        left: Val::Px(0.0),
        top: Val::Px(0.0),
        row_gap: Val::Px(10.0),
        ..default()
    }
}

/// The coin and the count, anchored in the window's bottom-right corner.
///
/// Absolutely positioned and inset by `inset`, which puts it inside the host window's own
/// padding rather than against its edge: an absolute child's inset is measured from the
/// padding box, so a zero here would sit the coin on the frame. The inset is a parameter
/// because there are two windows with a purse in the corner now — this one and the stall of
/// #459 — and they are padded differently.
///
/// `FocusPolicy::Pass` on the container is load-bearing and the omission is silent — a node
/// with no policy blocks — and it is the same trap `ui::icon::host_bundle` records: this
/// overlays the bottom of whichever tab panel is showing, and a readout that took the pointer
/// would make a strip of that panel unclickable with nothing to see.
pub(super) fn spawn_silver_readout(panel: &mut ChildSpawnerCommands<'_>, inset: f32) {
    panel
        .spawn((
            SilverReadout,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(inset),
                bottom: Val::Px(inset),
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                ..default()
            },
            FocusPolicy::Pass,
        ))
        .with_children(|readout| {
            readout
                .spawn((
                    Node {
                        position_type: PositionType::Relative,
                        width: Val::Px(SILVER_ICON_SIZE),
                        height: Val::Px(SILVER_ICON_SIZE),
                        flex_shrink: 0.0,
                        ..default()
                    },
                    FocusPolicy::Pass,
                ))
                .with_children(|host| {
                    // `None` for the liveries: silver wears none, so there is no resource to
                    // reach for and nothing about this picture that could sample one.
                    icon::spawn(host, silver_icon(), None);
                });
            readout.spawn((
                SilverCount,
                Text::new("0"),
                TextFont {
                    font_size: FontSize::Px(COUNT_FONT_SIZE),
                    ..default()
                },
                TextColor(SILVER_TEXT),
                TextShadow::default(),
                FocusPolicy::Pass,
            ));
        });
}

/// Writes what the last authoritative state holds, and only when it has changed.
///
/// [`Inventory::silver`] is the purse carried beside the complete slot vector. Current
/// server states never put the reserved silver id in that vector; persisted invalid records
/// are refused server-side, while this presentation remains faithful to the dedicated count.
pub(super) fn refresh_silver_readout(
    inventory: Option<Res<Inventory>>,
    mut counts: Query<&mut Text, With<SilverCount>>,
) {
    let Some(inventory) = inventory else {
        return;
    };
    let label = inventory.silver().to_string();
    for mut text in &mut counts {
        if text.0 != label {
            text.0.clone_from(&label);
        }
    }
}

/// Builds the four local shelves above the bounded recipe viewport.
fn spawn_craft_filter_strip(panel: &mut ChildSpawnerCommands<'_>) {
    panel
        .spawn((
            CraftFilterStrip,
            Node {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                flex_shrink: 0.0,
                column_gap: Val::Px(6.0),
                ..default()
            },
        ))
        .with_children(|strip| {
            for filter in CraftFilter::ALL {
                strip
                    .spawn((
                        CraftFilterButton(filter),
                        Button,
                        Node {
                            padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                            border_radius: BorderRadius::all(Val::Px(4.0)),
                            ..default()
                        },
                        BackgroundColor(BUTTON),
                    ))
                    .with_child((
                        Text::new(filter.label()),
                        TextFont {
                            font_size: FontSize::Px(14.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                    ));
            }
        });
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
                BackgroundColor(BUTTON),
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
/// granted. A craft made too far from its station is refused there, in silence.
fn station_note(station: StructureKind) -> String {
    let name = match station {
        StructureKind::Forge => "forge",
        StructureKind::Tent => "tent",
        StructureKind::Campfire => "campfire",
        StructureKind::Runestone => "runestone",
    };
    format!("requires a {name} nearby")
}

/// The line of help under a tab's contents.
///
/// One per tab rather than one for the screen, and the words are the ones that were on the
/// single line before #177 — split so each half sits under the controls it describes,
/// nothing added and nothing dropped.
fn hint(text: &str) -> impl Bundle {
    (
        Text::new(text),
        TextFont {
            font_size: FontSize::Px(15.0),
            ..default()
        },
        TextColor(Color::srgb(0.72, 0.75, 0.80)),
    )
}

/// The strip of tabs, built from [`InventoryTab::ALL`] so a third one appears in it by
/// existing.
fn spawn_tab_strip(panel: &mut ChildSpawnerCommands<'_>) {
    panel
        .spawn((
            InventoryTabStrip,
            Node {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                flex_shrink: 0.0,
                column_gap: Val::Px(6.0),
                ..default()
            },
        ))
        .with_children(|strip| {
            for tab in InventoryTab::ALL {
                strip
                    .spawn((
                        TabButton(tab),
                        Button,
                        Node {
                            padding: UiRect::axes(Val::Px(14.0), Val::Px(7.0)),
                            border_radius: BorderRadius::all(Val::Px(4.0)),
                            ..default()
                        },
                        BackgroundColor(BUTTON),
                    ))
                    .with_child((
                        Text::new(tab.label()),
                        TextFont {
                            font_size: FontSize::Px(18.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                    ));
            }
        });
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

/// The pack and equipment are two columns inside the existing Pack tab.
fn pack_and_equipment_node() -> Node {
    Node {
        display: Display::Flex,
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::FlexStart,
        width: Val::Percent(100.0),
        column_gap: Val::Px(PACK_EQUIPMENT_GAP),
        ..default()
    }
}

/// The ordinary slots consume the width left beside the fixed equipment column.
fn pack_column_node() -> Node {
    Node {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        min_width: Val::Px(0.0),
        row_gap: Val::Px(10.0),
        ..default()
    }
}

fn equipment_column_node() -> Node {
    Node {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        width: Val::Px(EQUIPMENT_COLUMN_WIDTH),
        flex_shrink: 0.0,
        align_items: AlignItems::Center,
        row_gap: Val::Px(10.0),
        ..default()
    }
}

fn equipment_caption(offset: u8) -> &'static str {
    EQUIPMENT_CAPTIONS
        .get(usize::from(offset))
        .copied()
        .unwrap_or("WORN")
}

/// A small body-location label under no icon and no count.
///
/// `FocusPolicy::Pass` is load-bearing: without it only an empty equipment cell would stop
/// answering the pointer, because a full cell hides this child.
fn equipment_caption_bundle(label: &'static str) -> impl Bundle {
    (
        EquipmentCaption(label),
        Text::new(label),
        TextFont {
            font_size: FontSize::Px(10.0),
            ..default()
        },
        TextColor(Color::srgb(0.62, 0.66, 0.74)),
        FocusPolicy::Pass,
    )
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

    let equipment_first = session.0.inventory_slots - session.0.equipment_slots;
    for (grid_entity, grid, mut node) in &mut grids {
        let columns = match grid {
            InventoryGrid::Equipment => 1,
            InventoryGrid::Pack | InventoryGrid::Hotbar => u16::from(session.0.hotbar_slots),
        };
        node.grid_template_columns = RepeatedGridTrack::flex(columns, 1.0);
        let range = match grid {
            InventoryGrid::Pack => session.0.hotbar_slots..equipment_first,
            InventoryGrid::Hotbar => 0..session.0.hotbar_slots,
            InventoryGrid::Equipment => equipment_first..session.0.inventory_slots,
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
                .with_children(|cell| {
                    // The picture and the count are the same two children every other
                    // inventory cell gets. Equipment adds only the empty-state caption.
                    spawn_cell_contents(cell);
                    if *grid == InventoryGrid::Equipment {
                        cell.spawn(equipment_caption_bundle(equipment_caption(
                            slot - equipment_first,
                        )));
                    }
                });
        }
    }
}

/// Whether the routing mirror says one picked item does not belong in this equipment cell.
///
/// This answer colours a target and nothing else. The click path never reads it, so a wrong
/// table entry cannot suppress a request or grant a move; the server re-decides both cases.
fn equipment_target_is_refused(
    cell: &InventoryCell,
    interaction: Interaction,
    equipment_first: u8,
    picked: Option<InventoryStack>,
) -> bool {
    if cell.grid != InventoryGrid::Equipment || interaction == Interaction::None {
        return false;
    }
    let Some(stack) = picked.filter(|stack| stack.item_id != 0 && stack.count != 0) else {
        return false;
    };
    let Some(offset) = cell.slot.checked_sub(equipment_first) else {
        return false;
    };

    !equipment_item_fits(stack.item_id, offset)
}

fn inventory_cell_edge(
    cell: &InventoryCell,
    interaction: Interaction,
    equipment_first: u8,
    picked_slot: Option<u8>,
    picked_stack: Option<InventoryStack>,
) -> Color {
    if picked_slot == Some(cell.slot) {
        SELECTED_EDGE
    } else if equipment_target_is_refused(cell, interaction, equipment_first, picked_stack) {
        REFUSED_TINT
    } else {
        CELL_EDGE
    }
}

/// Redraws every pack, hotbar and equipment cell against the newest authoritative slot.
///
/// The plate and the picked-up edge belong to this screen; what goes *inside* the cell is
/// [`refresh_cell_contents`], shared with the always-visible hotbar so one stack cannot be
/// drawn two ways. `Without<SlotCount>` on the cells is what keeps the two
/// `BackgroundColor` queries disjoint — the cell's plate and the count's plate are the same
/// component on two entities.
fn refresh_inventory_cells(
    mut commands: Commands,
    state: InventoryCellState<'_>,
    mut cells: Query<
        (
            &InventoryCell,
            &Interaction,
            &Children,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        Without<SlotCount>,
    >,
    mut counts: Query<(&mut Text, &mut BackgroundColor), With<SlotCount>>,
    mut icons: Query<&mut DrawnIcon>,
    mut captions: Query<(&EquipmentCaption, &mut Visibility)>,
    // Optional, because the UI stands up headlessly without the player plugin that owns it.
    liveries: Option<Res<Liveries>>,
) {
    let (Some(session), Some(inventory), Some(picked)) =
        (state.session, state.inventory, state.picked)
    else {
        return;
    };
    let equipment_first = session.0.inventory_slots - session.0.equipment_slots;
    let picked_stack = picked.slot().and_then(|slot| inventory.slot(slot));

    for (cell, interaction, children, mut background, mut border) in &mut cells {
        let style = stack_style(inventory.slot(cell.slot));
        if background.0 != style.background {
            background.0 = style.background;
        }
        let edge = inventory_cell_edge(
            cell,
            *interaction,
            equipment_first,
            picked.slot(),
            picked_stack,
        );
        let next = BorderColor::all(edge);
        if *border != next {
            *border = next;
        }
        refresh_cell_contents(
            &mut commands,
            children,
            &style,
            &mut counts,
            &mut icons,
            liveries.as_deref(),
        );
        for child in children {
            if let Ok((caption, mut visibility)) = captions.get_mut(*child) {
                debug_assert!(!caption.0.is_empty());
                let next = if style.icon.is_none() {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                if *visibility != next {
                    *visibility = next;
                }
            }
        }
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
        // Short rows ignore hover as well as the press: a row that cannot be used should
        // read as inert rather than as one that failed to respond. That is this row's own
        // state, decided here; the ordinary three come from the one place every other
        // pressable thing reads them.
        let next = if row.0.affordable(&inventory) {
            button_colour(interaction)
        } else {
            RECIPE_ROW_SHORT
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
            REFUSED_TINT
        });
        if *colour != next {
            *colour = next;
        }
    }
}

/// Reads the tab strip, and paints it.
///
/// **Originates nothing.** A press writes a resource; there is no message, no request and
/// nothing the server could learn from it. The screen is a mirror on both tabs.
fn switch_tabs(
    mode: Res<InputMode>,
    mut tabs: Query<(&TabButton, &Interaction, &mut BackgroundColor)>,
    mut active: ResMut<InventoryTab>,
) {
    // The same guard `inventory_clicks` and `craft_clicks` keep, and it is load-bearing
    // rather than tidy: without it a tab left under the pointer as the screen closes goes
    // on selecting itself every frame, and would undo the reset `show_inventory` performs
    // when the screen next opens. Found by `re_opening_the_screen_returns_to_the_pack`.
    if *mode != InputMode::Inventory {
        return;
    }
    for (tab, interaction, _) in &tabs {
        if *interaction == Interaction::Pressed && *active != tab.0 {
            *active = tab.0;
        }
    }

    for (tab, interaction, mut colour) in &mut tabs {
        // The selected tab keeps its own colour under the pointer too: a tab that lit up
        // like an unselected one while hovered would read as though pressing it did
        // something, and it does not.
        let next = if tab.0 == *active {
            TAB_SELECTED
        } else {
            button_colour(interaction)
        };
        if colour.0 != next {
            colour.0 = next;
        }
    }
}

/// Gives the active tab a `Display` and takes it from the others.
///
/// `Display`, never `Visibility` — see [`TabPanel`].
fn show_the_active_tab(active: Res<InventoryTab>, mut panels: Query<(&TabPanel, &mut Node)>) {
    for (panel, mut node) in &mut panels {
        let next = if panel.0 == *active {
            Display::Flex
        } else {
            Display::None
        };
        // Written only on a change, because `Mut<Node>` marks the component changed on the
        // first `DerefMut` and `bevy_ui` lays a changed node's subtree out again.
        if node.display != next {
            node.display = next;
        }
    }
}

/// Replaces the rows wholesale from the newest complete server answer.
fn rebuild_mount_rows(
    mut commands: Commands,
    learned: Option<Res<LearnedMounts>>,
    settings: Option<Res<Settings>>,
    lists: Query<(Entity, Option<&Children>), With<MountList>>,
) {
    let changed = learned.as_ref().is_some_and(|mounts| mounts.is_changed())
        || settings
            .as_ref()
            .is_some_and(|settings| settings.is_changed());
    for (list, children) in &lists {
        if !changed && children.is_some() {
            continue;
        }
        if children.is_some() {
            commands.entity(list).despawn_related::<Children>();
        }
        let mounts = learned.as_deref().map_or(&[][..], LearnedMounts::mounts);
        commands.entity(list).with_children(|rows| {
            if mounts.is_empty() {
                rows.spawn((
                    Text::new("You have not learned any mounts yet"),
                    TextFont {
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.72, 0.75, 0.80)),
                ));
                return;
            }
            let selected = settings.as_deref().and_then(Settings::default_mount);
            for mount in mounts {
                let is_default = selected == Some(preference_from_mount(*mount));
                rows.spawn((
                    MountChoice(*mount),
                    Button,
                    Node {
                        width: Val::Percent(100.0),
                        padding: UiRect::all(Val::Px(12.0)),
                        border_radius: BorderRadius::all(Val::Px(4.0)),
                        ..default()
                    },
                    BackgroundColor(if is_default { TAB_SELECTED } else { BUTTON }),
                ))
                .with_child((
                    Text::new(if is_default {
                        format!("{}  -  DEFAULT", mount_label(*mount))
                    } else {
                        mount_label(*mount).to_owned()
                    }),
                    TextFont {
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));
            }
        });
    }
}

fn choose_default_mount(
    mode: Res<InputMode>,
    active: Res<InventoryTab>,
    learned: Option<Res<LearnedMounts>>,
    mut settings: Option<ResMut<Settings>>,
    choices: Query<(&MountChoice, &Interaction)>,
) {
    if *mode != InputMode::Inventory || *active != InventoryTab::Mounts {
        return;
    }
    let (Some(learned), Some(settings)) = (learned, settings.as_deref_mut()) else {
        return;
    };
    for (choice, interaction) in &choices {
        if *interaction == Interaction::Pressed && learned.contains(choice.0) {
            settings.set_default_mount(preference_from_mount(choice.0));
        }
    }
}

/// Selects one presentational recipe shelf and paints the strip from the shared palette.
fn switch_craft_filters(
    mode: Res<InputMode>,
    active_tab: Res<InventoryTab>,
    mut buttons: Query<(&CraftFilterButton, &Interaction, &mut BackgroundColor)>,
    mut active_filter: ResMut<CraftFilter>,
    mut scroll_areas: Query<&mut ScrollPosition, With<CraftScrollArea>>,
) {
    if *mode != InputMode::Inventory || *active_tab != InventoryTab::Crafting {
        return;
    }

    let mut changed = false;
    for (button, interaction, _) in &buttons {
        if *interaction == Interaction::Pressed && *active_filter != button.0 {
            *active_filter = button.0;
            changed = true;
        }
    }
    if changed {
        for mut position in &mut scroll_areas {
            if position.0 != Vec2::ZERO {
                position.0 = Vec2::ZERO;
            }
        }
    }

    for (button, interaction, mut colour) in &mut buttons {
        let next = if button.0 == *active_filter {
            TAB_SELECTED
        } else {
            button_colour(interaction)
        };
        if colour.0 != next {
            colour.0 = next;
        }
    }
}

/// Takes rows outside the selected shelf out of layout entirely.
fn show_filtered_recipes(active: Res<CraftFilter>, mut rows: Query<(&CraftRow, &mut Node)>) {
    for (row, mut node) in &mut rows {
        let next = if active.includes(row.0.category) {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != next {
            node.display = next;
        }
    }
}

/// Returns a bounded logical scroll offset after one wheel movement.
fn scrolled_offset(
    current: f32,
    wheel: f32,
    unit: MouseScrollUnit,
    visible: f32,
    content: f32,
) -> f32 {
    let delta = match unit {
        MouseScrollUnit::Line => -wheel * CRAFT_SCROLL_LINE,
        MouseScrollUnit::Pixel => -wheel,
    };
    bounded_scroll_offset(current + delta, visible, content)
}

/// Keeps an existing offset inside the bounds reported by the latest UI layout.
fn bounded_scroll_offset(current: f32, visible: f32, content: f32) -> f32 {
    current.clamp(0.0, (content - visible).max(0.0))
}

/// Scrolls the active recipe shelf. The inventory owns the whole pointer while it is open,
/// so the wheel may act anywhere on the crafting tab rather than only over a row.
fn scroll_crafting(
    mode: Res<InputMode>,
    active_tab: Res<InventoryTab>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    mut areas: Query<(&mut ScrollPosition, &ComputedNode), With<CraftScrollArea>>,
) {
    if *mode != InputMode::Inventory || *active_tab != InventoryTab::Crafting {
        return;
    }
    let Some(scroll) = scroll.filter(|scroll| scroll.delta.y != 0.0) else {
        return;
    };
    for (mut position, computed) in &mut areas {
        let scale = computed.inverse_scale_factor;
        let visible = computed.size().y * scale;
        let content = computed.content_size().y * scale;
        let next = scrolled_offset(position.y, scroll.delta.y, scroll.unit, visible, content);
        if position.y != next {
            position.y = next;
        }
    }
}

/// Converts the viewport, content and rail sizes into a scrollbar thumb height and top.
fn scrollbar_geometry(scroll: f32, visible: f32, content: f32, rail: f32) -> (f32, f32) {
    if rail <= 0.0 || visible <= 0.0 || content <= visible {
        return (rail.max(0.0), 0.0);
    }
    let thumb = (rail * visible / content).clamp(CRAFT_SCROLLBAR_MIN_THUMB.min(rail), rail);
    let scroll_range = content - visible;
    let travel = rail - thumb;
    let top = scroll.clamp(0.0, scroll_range) / scroll_range * travel;
    (thumb, top)
}

/// Mirrors the current scroll into the visible rail without touching layout when nothing
/// moved. The bar is passive; wheel input remains the one scrolling gesture.
fn sync_craft_scrollbar(
    mut areas: Query<(&mut ScrollPosition, &ComputedNode), With<CraftScrollArea>>,
    rails: Query<&ComputedNode, With<CraftScrollbar>>,
    mut thumbs: Query<&mut Node, With<CraftScrollbarThumb>>,
) {
    let (Some((mut position, area)), Some(rail)) = (areas.iter_mut().next(), rails.iter().next())
    else {
        return;
    };
    let area_scale = area.inverse_scale_factor;
    let rail_scale = rail.inverse_scale_factor;
    let visible = area.size().y * area_scale;
    let content = area.content_size().y * area_scale;
    let bounded = bounded_scroll_offset(position.y, visible, content);
    if position.y != bounded {
        position.y = bounded;
    }
    let (height, top) = scrollbar_geometry(bounded, visible, content, rail.size().y * rail_scale);
    for mut thumb in &mut thumbs {
        let next_height = Val::Px(height);
        let next_top = Val::Px(top);
        if thumb.height != next_height {
            thumb.height = next_height;
        }
        if thumb.top != next_top {
            thumb.top = next_top;
        }
    }
}

/// Keeps enough of the grab bar in the current viewport to recover the window.
///
/// Horizontally the frame may go mostly off-screen, but never so far that the grab bar
/// cannot be caught again. Vertically the grab bar never leaves the viewport: content may
/// extend below a small window, while the control that moves it cannot.
fn clamp_window_position(position: Vec2, viewport: Vec2) -> Vec2 {
    let viewport = viewport.max(Vec2::ZERO);
    let grab_width = (INVENTORY_WINDOW_SIZE.x - 2.0 * INVENTORY_WINDOW_PADDING).max(0.0);
    let visible_width = MIN_VISIBLE_GRAB_WIDTH.min(grab_width).min(viewport.x);
    let minimum = Vec2::new(
        visible_width - (INVENTORY_WINDOW_SIZE.x - INVENTORY_WINDOW_PADDING),
        -INVENTORY_WINDOW_PADDING,
    );
    let maximum = Vec2::new(
        viewport.x - visible_width - INVENTORY_WINDOW_PADDING,
        (viewport.y - INVENTORY_WINDOW_PADDING - GRAB_AREA_HEIGHT).max(-INVENTORY_WINDOW_PADDING),
    );
    position.clamp(minimum, maximum)
}

/// The old centred position, now only the first position of the session.
fn centred_window_position(viewport: Vec2) -> Vec2 {
    clamp_window_position((viewport - INVENTORY_WINDOW_SIZE) / 2.0, viewport)
}

fn set_window_position(node: &mut Node, position: Vec2) {
    let (left, top) = (Val::Px(position.x), Val::Px(position.y));
    if node.left != left {
        node.left = left;
    }
    if node.top != top {
        node.top = top;
    }
}

/// Moves the inventory while the primary button remains held after a press on the grab
/// bar. Once begun, the drag follows the pointer even after it leaves the bar.
fn drag_inventory_window(
    mode: Res<InputMode>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    grabs: Query<&Interaction, With<InventoryGrabArea>>,
    mut position: ResMut<InventoryWindowPosition>,
    mut drag: ResMut<InventoryDrag>,
    mut frames: Query<&mut Node, With<InventoryWindow>>,
) {
    let held = buttons
        .as_deref()
        .is_some_and(|buttons| buttons.pressed(MouseButton::Left));
    if *mode != InputMode::Inventory || !held {
        if drag.0.is_some() {
            drag.0 = None;
        }
        return;
    }

    let Some(window) = windows.iter().next() else {
        if drag.0.is_some() {
            drag.0 = None;
        }
        return;
    };
    let viewport = Vec2::new(window.width(), window.height());
    let Some(cursor) = window.cursor_position() else {
        // A release outside the window is not delivered on every platform. Losing the
        // cursor therefore ends the gesture here, so re-entering cannot resume an old
        // cursor offset with a button state the window never got to clear.
        if drag.0.is_some() {
            drag.0 = None;
        }
        return;
    };
    let current = position
        .0
        .unwrap_or_else(|| centred_window_position(viewport));

    if drag.0.is_none()
        && grabs
            .iter()
            .any(|interaction| *interaction == Interaction::Pressed)
    {
        drag.0 = Some(cursor - current);
    }
    let Some(offset) = drag.0 else {
        return;
    };

    let next = clamp_window_position(cursor - offset, viewport);
    if position.0 != Some(next) {
        position.0 = Some(next);
    }
    for mut frame in &mut frames {
        set_window_position(&mut frame, next);
    }
}

fn show_inventory(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut active: ResMut<InventoryTab>,
    mut roots: Query<&mut Visibility, With<InventoryRoot>>,
) {
    let next = if *mode == InputMode::Inventory && session.is_some() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut roots {
        if *visibility != next {
            // **Opening resets the tab**, which is what "Pack is the one it opens on"
            // means for the second opening as well as the first. Inside the change guard,
            // so a screen that is already up is left where the player put it.
            if next == Visibility::Visible && *active != InventoryTab::Pack {
                *active = InventoryTab::Pack;
            }
            *visibility = next;
        }
    }
}

/// Restores the session position on an opening and clamps it against the viewport that
/// exists now, not the one in which it was last dragged.
fn place_inventory_window_on_open(
    roots: Query<&Visibility, (With<InventoryRoot>, Changed<Visibility>)>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut position: ResMut<InventoryWindowPosition>,
    mut drag: ResMut<InventoryDrag>,
    mut frames: Query<&mut Node, With<InventoryWindow>>,
) {
    let Some(visibility) = roots.iter().next() else {
        return;
    };
    if *visibility != Visibility::Visible {
        // Closing while held ends that gesture; reopening cannot inherit a stale cursor
        // offset even though it deliberately inherits the position.
        if drag.0.is_some() {
            drag.0 = None;
        }
        return;
    }

    let Some(window) = windows.iter().next() else {
        return;
    };
    let viewport = Vec2::new(window.width(), window.height());
    let wanted = position
        .0
        .unwrap_or_else(|| centred_window_position(viewport));
    let placed = clamp_window_position(wanted, viewport);
    if position.0 != Some(placed) {
        position.0 = Some(placed);
    }
    for mut frame in &mut frames {
        set_window_position(&mut frame, placed);
    }
}

/// Keeps an already-open inventory recoverable when the viewport becomes smaller.
///
/// Opening and dragging already clamp through the current viewport. A resize is the third
/// way that viewport can change around a stationary frame, so it must update both the
/// remembered position and the node rather than waiting for a close and reopen.
fn reclamp_inventory_window_on_resize(
    mut resized: MessageReader<WindowResized>,
    primary: Query<Entity, With<PrimaryWindow>>,
    roots: Query<&Visibility, With<InventoryRoot>>,
    mut position: ResMut<InventoryWindowPosition>,
    mut frames: Query<&mut Node, With<InventoryWindow>>,
) {
    let Some(primary) = primary.iter().next() else {
        return;
    };
    let viewport = resized
        .read()
        .filter(|event| event.window == primary)
        .map(|event| Vec2::new(event.width, event.height))
        .last();
    let Some(viewport) = viewport else {
        return;
    };
    if roots.iter().next() != Some(&Visibility::Visible) {
        return;
    }

    let wanted = position
        .0
        .unwrap_or_else(|| centred_window_position(viewport));
    let placed = clamp_window_position(wanted, viewport);
    if position.0 != Some(placed) {
        position.0 = Some(placed);
    }
    for mut frame in &mut frames {
        set_window_position(&mut frame, placed);
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

    let pointer = pointer_in_window(&windows);

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
        anchor_for(cursor, window).apply_to(&mut node);
    }
}

/// Reports one press per cell, and says which of the four things it was asking for.
///
/// **Nothing is decided here.** A reported press becomes an `InventoryMoveRequest`, a
/// `RepairRequest`, a `DropItemRequest` or a `ConsumeRequest` in `player::inventory`, which
/// is the only module that pairs cells, checks its routing lists and builds a frame. That
/// is what keeps the keyboard route below from being a second opinion about consumables: whether
/// the hovered cell holds something consumable is `consume_request`'s question, and this system
/// does not ask it for either gesture.
///
/// **Shift is read against the full-stack button and not the split one**, because what the
/// modifier changes is *where the stack goes* rather than *how much of it moves*: a drop is
/// the whole cell, exactly as a plain left press picks the whole cell. Right-clicking keeps
/// meaning half with shift held or without. Consume is the independent gesture and
/// therefore never becomes a source or destination for either one.
///
/// **Consume has two spellings and one meaning.** [`Control::Consume`] is the rebindable
/// one, read through the same [`Bindings`] every other control is; middle-click is the
/// fixed shortcut it grew out of, kept because a player who learned it should not have to
/// unlearn it. They share one branch rather than one each, so pressing both in the same
/// frame over the same cell reports the press once — two branches would report it twice and
/// `player::inventory` would build two frames for one intent. The key is edge-triggered
/// (`just_pressed`) for the same reason the buttons are: a held key is one press.
///
/// An absent [`Settings`] falls back to [`Bindings::default`], the pattern `player/loot.rs`
/// already follows, so a headless app built without the resource still routes the default
/// key rather than routing nothing.
fn inventory_clicks(
    mode: Res<InputMode>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    settings: Option<Res<Settings>>,
    cells: Query<(&Interaction, &InventoryCell)>,
    mut clicks: MessageWriter<InventoryClick>,
) {
    if *mode != InputMode::Inventory {
        return;
    }
    let Some(buttons) = buttons else {
        return;
    };
    let keys = keys.as_deref();
    let bindings = settings
        .as_deref()
        .map_or_else(Bindings::default, |settings| *settings.bindings());
    let dropping = keys
        .is_some_and(|keys| keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight));
    let consuming = keys.is_some_and(|keys| keys.just_pressed(bindings.key(Control::Consume)));
    let kind = if buttons.just_pressed(MouseButton::Middle) || consuming {
        Some(InventoryClickKind::Consume)
    } else if buttons.just_pressed(MouseButton::Right) {
        Some(InventoryClickKind::Split)
    } else if buttons.just_pressed(MouseButton::Left) {
        Some(if dropping {
            InventoryClickKind::Drop
        } else {
            InventoryClickKind::Full
        })
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
    use bevy::ecs::system::RunSystemOnce;
    use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
    use bevy::input::{ButtonState, InputPlugin};

    use super::super::{COUNT_PLATE, DrawnCell, FILLED_CELL, drawn_cell, icon};
    use super::*;
    use crate::net::{InventoryStack, MountKind, RecipeId, SessionParams};
    use crate::player::{ITEM_SILVER, ItemShape, SelectedSlot};
    use crate::settings::DefaultMount;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 7,
            hotbar_slots: 2,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 0.0,
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
    fn learned_and_default_mount_rows_use_the_canonical_breed_names() {
        let mut app = app();
        app.insert_resource(LearnedMounts::for_test(vec![
            MountKind::BlackHorse,
            MountKind::BrownHorse,
            MountKind::GreyHorse,
        ]));
        let mut settings = Settings::default();
        settings.set_default_mount(DefaultMount::Brown);
        app.insert_resource(settings);
        app.update();

        let world = app.world_mut();
        let mut texts = world.query::<&Text>();
        let lines: Vec<_> = texts.iter(world).map(|text| text.0.clone()).collect();
        assert!(lines.contains(&"Raven Friesian".to_owned()), "{lines:?}");
        assert!(
            lines.contains(&"Chestnut Icelandic  -  DEFAULT".to_owned()),
            "{lines:?}"
        );
        assert!(lines.contains(&"Silver Fjord".to_owned()), "{lines:?}");
        assert!(
            !lines.iter().any(|line| line.contains("unknown item")),
            "a learned mount still fell through the registry: {lines:?}"
        );
    }

    /// Which tabs have a `Display`, read the way `bevy_ui` decides whether a half occupies
    /// the panel.
    ///
    /// `Display` and not `Visibility`, which is the trap: a hidden node is laid out exactly
    /// as a visible one, so a `Visibility` here would leave the crafting rows taking up the
    /// panel's height behind the pack. `ui/character.rs` records the same thing for its two
    /// halves.
    fn shown(app: &mut App) -> Vec<InventoryTab> {
        let world = app.world_mut();
        let mut query = world.query::<(&TabPanel, &Node)>();
        let mut tabs: Vec<InventoryTab> = query
            .iter(world)
            .filter(|(_, node)| node.display != Display::None)
            .map(|(panel, _)| panel.0)
            .collect();
        tabs.sort_by_key(|tab| format!("{tab:?}"));
        tabs
    }

    fn tab_button(app: &mut App, tab: InventoryTab) -> Entity {
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &TabButton)>();
        query
            .iter(world)
            .find(|(_, button)| button.0 == tab)
            .map(|(entity, _)| entity)
            .expect("every tab has a button")
    }

    fn tab_colour(app: &mut App, tab: InventoryTab) -> Color {
        let button = tab_button(app, tab);
        app.world()
            .get::<BackgroundColor>(button)
            .expect("a tab is drawn")
            .0
    }

    /// A valid seven-slot state whose money exists only in the dedicated purse.
    ///
    /// Every slot is empty deliberately: a non-zero balance must not manufacture a stack
    /// merely so the existing slot renderer can draw it.
    fn pursed_inventory() -> Inventory {
        Inventory::from_state(vec![InventoryStack::default(); 7], 41)
    }

    fn silver_readout(app: &mut App) -> Entity {
        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<SilverReadout>>();
        query.single(world).expect("exactly one silver readout")
    }

    fn silver_label(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<SilverCount>>();
        query
            .single(world)
            .expect("exactly one silver count")
            .0
            .clone()
    }

    fn inventory_window(app: &mut App) -> Entity {
        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<InventoryWindow>>();
        query.single(world).expect("exactly one inventory window")
    }

    fn grab_area(app: &mut App) -> Entity {
        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<InventoryGrabArea>>();
        query
            .single(world)
            .expect("exactly one inventory grab area")
    }

    fn position(app: &App) -> Vec2 {
        app.world()
            .resource::<InventoryWindowPosition>()
            .0
            .expect("an opened inventory has a position")
    }

    fn add_window(app: &mut App, size: UVec2, cursor: Vec2) -> Entity {
        let mut window = Window {
            resolution: bevy::window::WindowResolution::new(size.x, size.y),
            ..default()
        };
        window.set_cursor_position(Some(cursor));
        app.world_mut().spawn((PrimaryWindow, window)).id()
    }

    #[test]
    fn the_screen_opens_on_the_pack_and_shows_one_tab_at_a_time() {
        let mut app = app();
        app.update();

        assert_eq!(shown(&mut app), vec![InventoryTab::Pack]);
        assert_eq!(
            *app.world().resource::<InventoryTab>(),
            InventoryTab::Pack,
            "the screen opened on something else"
        );
    }

    #[test]
    fn pressing_a_tab_swaps_which_half_occupies_the_panel() {
        let mut app = app();
        app.update();

        let crafting = tab_button(&mut app, InventoryTab::Crafting);
        *app.world_mut()
            .entity_mut(crafting)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::Pressed;
        app.update();

        assert_eq!(shown(&mut app), vec![InventoryTab::Crafting]);

        let pack = tab_button(&mut app, InventoryTab::Pack);
        *app.world_mut()
            .entity_mut(crafting)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::None;
        *app.world_mut()
            .entity_mut(pack)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::Pressed;
        app.update();

        assert_eq!(shown(&mut app), vec![InventoryTab::Pack]);
    }

    /// The regression this issue closes, asserted on the layout inputs rather than a
    /// screenshot: the frame has one explicit size and both tab bodies occupy the same
    /// absolute origin. A tab body whose natural height participated here would make one
    /// of these properties false and move the strip when the centred frame was laid out.
    #[test]
    fn the_tab_strip_keeps_one_layout_when_the_content_switches() {
        let mut app = app();
        add_window(&mut app, UVec2::new(1280, 900), Vec2::new(640.0, 450.0));
        app.update();

        let frame = inventory_window(&mut app);
        let before = app
            .world()
            .get::<Node>(frame)
            .expect("the frame is a node")
            .clone();
        let before_position = position(&app);

        let crafting = tab_button(&mut app, InventoryTab::Crafting);
        *app.world_mut()
            .entity_mut(crafting)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::Pressed;
        app.update();

        let after = app.world().get::<Node>(frame).expect("the frame is a node");
        assert_eq!(
            position(&app),
            before_position,
            "the frame moved with its content"
        );
        assert_eq!(after.left, before.left);
        assert_eq!(after.top, before.top);
        assert_eq!(after.width, Val::Px(INVENTORY_WINDOW_SIZE.x));
        assert_eq!(after.height, Val::Px(INVENTORY_WINDOW_SIZE.y));

        let world = app.world_mut();
        let mut strips = world.query_filtered::<&Node, With<InventoryTabStrip>>();
        assert_eq!(strips.iter(world).count(), 1, "the strip was replaced");
        let mut panels = world.query::<(&TabPanel, &Node)>();
        for (_, node) in panels.iter(world) {
            assert_eq!(node.position_type, PositionType::Absolute);
            assert_eq!((node.left, node.top), (Val::Px(0.0), Val::Px(0.0)));
            assert_eq!(node.width, Val::Percent(100.0));
        }
    }

    #[test]
    fn dragging_moves_by_the_pointer_delta_and_release_leaves_it_there() {
        let mut app = app();
        app.insert_resource(ButtonInput::<MouseButton>::default());
        let window = add_window(&mut app, UVec2::new(1280, 900), Vec2::new(640.0, 200.0));
        app.update();
        let start = position(&app);

        let grab = grab_area(&mut app);
        *app.world_mut()
            .entity_mut(grab)
            .get_mut::<Interaction>()
            .expect("the grab area is a button") = Interaction::Pressed;
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        app.update();

        app.world_mut()
            .entity_mut(window)
            .get_mut::<Window>()
            .expect("the primary window exists")
            .set_cursor_position(Some(Vec2::new(760.0, 275.0)));
        app.update();
        assert_eq!(position(&app), start + Vec2::new(120.0, 75.0));

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .release(MouseButton::Left);
        app.update();
        let released = position(&app);
        assert_eq!(app.world().resource::<InventoryDrag>().0, None);

        app.world_mut()
            .entity_mut(window)
            .get_mut::<Window>()
            .expect("the primary window exists")
            .set_cursor_position(Some(Vec2::new(900.0, 400.0)));
        app.update();
        assert_eq!(
            position(&app),
            released,
            "a released drag kept following the pointer"
        );
    }

    #[test]
    fn losing_the_cursor_ends_a_drag_that_may_never_receive_its_release() {
        let mut app = app();
        app.insert_resource(ButtonInput::<MouseButton>::default());
        let window = add_window(&mut app, UVec2::new(1280, 900), Vec2::new(640.0, 200.0));
        app.update();

        let grab = grab_area(&mut app);
        *app.world_mut()
            .entity_mut(grab)
            .get_mut::<Interaction>()
            .expect("the grab area is a button") = Interaction::Pressed;
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        app.update();
        assert!(app.world().resource::<InventoryDrag>().0.is_some());

        *app.world_mut()
            .entity_mut(grab)
            .get_mut::<Interaction>()
            .expect("the grab area is a button") = Interaction::None;
        app.world_mut()
            .entity_mut(window)
            .get_mut::<Window>()
            .expect("the primary window exists")
            .set_cursor_position(None);
        app.update();
        assert_eq!(app.world().resource::<InventoryDrag>().0, None);
        let left_outside = position(&app);

        // Model a platform that never delivered the release: the stale held state cannot
        // make the old gesture resume when the pointer comes back.
        app.world_mut()
            .entity_mut(window)
            .get_mut::<Window>()
            .expect("the primary window exists")
            .set_cursor_position(Some(Vec2::new(900.0, 400.0)));
        app.update();
        assert_eq!(position(&app), left_outside);
        assert_eq!(app.world().resource::<InventoryDrag>().0, None);
    }

    #[test]
    fn the_drag_clamp_keeps_the_grab_area_recoverable_at_every_edge() {
        let viewport = Vec2::new(800.0, 600.0);
        assert_eq!(
            clamp_window_position(Vec2::splat(-10_000.0), viewport),
            Vec2::new(
                MIN_VISIBLE_GRAB_WIDTH - (INVENTORY_WINDOW_SIZE.x - INVENTORY_WINDOW_PADDING),
                -INVENTORY_WINDOW_PADDING,
            )
        );
        assert_eq!(
            clamp_window_position(Vec2::splat(10_000.0), viewport),
            Vec2::new(
                viewport.x - MIN_VISIBLE_GRAB_WIDTH - INVENTORY_WINDOW_PADDING,
                viewport.y - INVENTORY_WINDOW_PADDING - GRAB_AREA_HEIGHT,
            )
        );
    }

    #[test]
    fn reopening_keeps_the_session_position_and_reclamps_it_to_the_current_window() {
        let mut app = app();
        let window = add_window(&mut app, UVec2::new(1920, 1080), Vec2::new(0.0, 0.0));
        app.update();

        let left_on_large_window = Vec2::new(1500.0, 900.0);
        app.world_mut().resource_mut::<InventoryWindowPosition>().0 = Some(left_on_large_window);
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();

        app.world_mut()
            .entity_mut(window)
            .get_mut::<Window>()
            .expect("the primary window exists")
            .resolution = bevy::window::WindowResolution::new(800, 600);
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();

        let expected = clamp_window_position(left_on_large_window, Vec2::new(800.0, 600.0));
        assert_eq!(position(&app), expected);
        let frame = inventory_window(&mut app);
        let node = app.world().get::<Node>(frame).expect("the frame is a node");
        assert_eq!(
            (node.left, node.top),
            (Val::Px(expected.x), Val::Px(expected.y))
        );
    }

    #[test]
    fn resizing_reclamps_an_inventory_that_is_already_open() {
        let mut app = app();
        let window = add_window(&mut app, UVec2::new(1920, 1080), Vec2::new(0.0, 0.0));
        app.update();

        let left_on_large_window = Vec2::new(1500.0, 900.0);
        app.world_mut().resource_mut::<InventoryWindowPosition>().0 = Some(left_on_large_window);
        let frame = inventory_window(&mut app);
        set_window_position(
            &mut app
                .world_mut()
                .entity_mut(frame)
                .get_mut::<Node>()
                .expect("the frame is a node"),
            left_on_large_window,
        );
        app.world_mut()
            .entity_mut(window)
            .get_mut::<Window>()
            .expect("the primary window exists")
            .resolution = bevy::window::WindowResolution::new(800, 600);
        app.world_mut().write_message(WindowResized {
            window,
            width: 800.0,
            height: 600.0,
        });
        app.update();

        let expected = clamp_window_position(left_on_large_window, Vec2::new(800.0, 600.0));
        assert_eq!(position(&app), expected);
        let node = app.world().get::<Node>(frame).expect("the frame is a node");
        assert_eq!(
            (node.left, node.top),
            (Val::Px(expected.x), Val::Px(expected.y))
        );
    }

    /// **Switching tabs originates nothing**, which is the acceptance criterion that keeps
    /// this a refactor: the screen is a mirror on both halves, and a tab is which nodes have
    /// a `Display` rather than anything the server could learn.
    #[test]
    fn switching_tabs_reports_no_intent_and_changes_no_count() {
        let mut app = app();
        app.update();
        let before = app.world().resource::<Inventory>().stacks().to_vec();

        for tab in [InventoryTab::Crafting, InventoryTab::Pack] {
            let button = tab_button(&mut app, tab);
            *app.world_mut()
                .entity_mut(button)
                .get_mut::<Interaction>()
                .expect("a tab is a button") = Interaction::Pressed;
            app.update();
            *app.world_mut()
                .entity_mut(button)
                .get_mut::<Interaction>()
                .expect("a tab is a button") = Interaction::None;
        }

        let crafts: Vec<CraftClick> = app
            .world_mut()
            .resource_mut::<Messages<CraftClick>>()
            .drain()
            .collect();
        assert_eq!(crafts, vec![], "a tab press asked to craft something");
        let moves: Vec<InventoryClick> = app
            .world_mut()
            .resource_mut::<Messages<InventoryClick>>()
            .drain()
            .collect();
        assert_eq!(moves, vec![], "a tab press asked to move a stack");
        assert_eq!(
            app.world().resource::<Inventory>().stacks(),
            before,
            "a tab press changed a displayed count"
        );
    }

    /// Closing and re-opening puts the pack back, which is what "the one it opens on" means
    /// for the second opening as well as the first.
    #[test]
    fn re_opening_the_screen_returns_to_the_pack() {
        let mut app = app();
        app.update();

        let crafting = tab_button(&mut app, InventoryTab::Crafting);
        *app.world_mut()
            .entity_mut(crafting)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::Pressed;
        app.update();
        assert_eq!(shown(&mut app), vec![InventoryTab::Crafting]);

        // Released, which is what a player's hand does and what `bevy_ui` writes back on
        // the next frame. Left pressed, the stale interaction re-selects the tab on the
        // frame the screen re-opens — a property of poking the component directly rather
        // than of the screen.
        *app.world_mut()
            .entity_mut(crafting)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::None;

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();

        assert_eq!(shown(&mut app), vec![InventoryTab::Pack]);
    }

    /// **The tab strip is not a fifth copy of the button palette.**
    ///
    /// #163 collapsed four copies of the three button colours into one
    /// `ui::button_colour`, and a tab strip is exactly the next candidate. The unselected
    /// tab wears what that function answers; the selected one wears a colour of its own,
    /// which is the same shape the short recipe row keeps — a state the three-interaction
    /// palette has no arm for.
    #[test]
    fn a_tab_reads_the_shared_palette_and_the_selected_one_keeps_its_own_colour() {
        let mut app = app();
        app.update();

        assert_eq!(tab_colour(&mut app, InventoryTab::Pack), TAB_SELECTED);
        assert_eq!(tab_colour(&mut app, InventoryTab::Crafting), BUTTON);

        // And a hover on the selected tab does not light it like an unselected one: it
        // would read as though pressing it did something, and it does not.
        let pack = tab_button(&mut app, InventoryTab::Pack);
        *app.world_mut()
            .entity_mut(pack)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::Hovered;
        app.update();
        assert_eq!(tab_colour(&mut app, InventoryTab::Pack), TAB_SELECTED);

        // An unselected one does, through the shared function and not a copy of it.
        let crafting = tab_button(&mut app, InventoryTab::Crafting);
        *app.world_mut()
            .entity_mut(crafting)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::Hovered;
        app.update();
        assert_eq!(
            tab_colour(&mut app, InventoryTab::Crafting),
            button_colour(&Interaction::Hovered)
        );
    }

    #[test]
    fn every_inventory_slot_is_drawn_with_equipment_in_its_own_column() {
        let mut app = app();
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<&InventoryCell>();
        let mut cells: Vec<InventoryCell> = query.iter(world).copied().collect();
        cells.sort_by_key(|cell| cell.slot);
        assert_eq!(cells.len(), 7);
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Hotbar)
                .map(|cell| cell.slot)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Pack)
                .map(|cell| cell.slot)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Equipment)
                .map(|cell| cell.slot)
                .collect::<Vec<_>>(),
            vec![3, 4, 5, 6]
        );
    }

    #[test]
    fn empty_equipment_cells_name_the_body_location_and_a_full_one_draws_the_stack() {
        let mut app = app();
        app.update();

        let captions = |app: &mut App| {
            let world = app.world_mut();
            let mut cells = world.query::<(&InventoryCell, &Children)>();
            let mut result = Vec::new();
            for (cell, children) in cells.iter(world) {
                if cell.grid != InventoryGrid::Equipment {
                    continue;
                }
                let child = children
                    .iter()
                    .find(|child| world.get::<EquipmentCaption>(*child).is_some())
                    .expect("an equipment cell has one caption");
                result.push((
                    cell.slot,
                    world
                        .get::<Text>(child)
                        .expect("the caption has text")
                        .0
                        .clone(),
                    *world
                        .get::<Visibility>(child)
                        .expect("the caption has visibility"),
                    world.get::<FocusPolicy>(child).copied(),
                ));
            }
            result.sort_by_key(|row| row.0);
            result
        };

        assert_eq!(
            captions(&mut app),
            vec![
                (
                    3,
                    "HEAD".to_owned(),
                    Visibility::Inherited,
                    Some(FocusPolicy::Pass)
                ),
                (
                    4,
                    "CHEST".to_owned(),
                    Visibility::Inherited,
                    Some(FocusPolicy::Pass)
                ),
                (
                    5,
                    "LEGS".to_owned(),
                    Visibility::Inherited,
                    Some(FocusPolicy::Pass)
                ),
                (
                    6,
                    "OFF-HAND".to_owned(),
                    Visibility::Inherited,
                    Some(FocusPolicy::Pass)
                ),
            ]
        );

        let head = EQUIPMENT_ROUTES[0][0];
        deliver(
            &mut app,
            &[
                (STONE, 5),
                (0, 0),
                (0, 0),
                (head, 1),
                (0, 0),
                (0, 0),
                (0, 0),
            ],
        );
        assert_eq!(captions(&mut app)[0].2, Visibility::Hidden);
        assert_eq!(drawn(&mut app, 3).count, "1");
        assert!(!drawn(&mut app, 3).rectangles.is_empty());
    }

    #[test]
    fn a_new_session_rebuilds_the_hotbar_split_when_the_total_is_unchanged() {
        let mut app = app();
        app.update();

        let mut params = session().0;
        params.hotbar_slots = 3;
        app.insert_resource(Session(params));
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<&InventoryCell>();
        let cells: Vec<InventoryCell> = query.iter(world).copied().collect();
        assert_eq!(cells.len(), 7);
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Hotbar)
                .count(),
            3
        );
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Pack)
                .count(),
            0
        );
        assert_eq!(
            cells
                .iter()
                .filter(|cell| cell.grid == InventoryGrid::Equipment)
                .count(),
            4
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

    #[test]
    fn a_refused_equipment_tint_never_changes_the_full_move_intent() {
        let equipment_first = session().0.inventory_slots - session().0.equipment_slots;
        let head = EQUIPMENT_ROUTES[0][0];
        let chest = EQUIPMENT_ROUTES[1][0];
        let picked_head = Some(InventoryStack {
            item_id: head,
            count: 1,
            ..Default::default()
        });
        let head_cell = InventoryCell {
            slot: equipment_first,
            grid: InventoryGrid::Equipment,
        };
        let chest_cell = InventoryCell {
            slot: equipment_first + 1,
            grid: InventoryGrid::Equipment,
        };
        let pack_cell = InventoryCell {
            slot: 2,
            grid: InventoryGrid::Pack,
        };

        assert_eq!(
            inventory_cell_edge(
                &head_cell,
                Interaction::Hovered,
                equipment_first,
                Some(0),
                picked_head,
            ),
            CELL_EDGE,
            "matching head armour was tinted as refused"
        );
        assert_eq!(
            inventory_cell_edge(
                &chest_cell,
                Interaction::Hovered,
                equipment_first,
                Some(0),
                picked_head,
            ),
            REFUSED_TINT,
            "head armour over the chest cell was not tinted"
        );
        assert_eq!(
            inventory_cell_edge(
                &pack_cell,
                Interaction::Hovered,
                equipment_first,
                Some(0),
                picked_head,
            ),
            CELL_EDGE,
            "an ordinary pack cell inherited the equipment courtesy"
        );
        assert_eq!(
            inventory_cell_edge(
                &chest_cell,
                Interaction::Hovered,
                equipment_first,
                Some(0),
                Some(InventoryStack {
                    item_id: chest,
                    count: 1,
                    ..Default::default()
                }),
            ),
            CELL_EDGE,
            "matching chest armour was tinted as refused"
        );
        assert_eq!(
            inventory_cell_edge(
                &chest_cell,
                Interaction::Hovered,
                equipment_first,
                Some(chest_cell.slot),
                picked_head,
            ),
            SELECTED_EDGE,
            "the picked source lost its selected edge to the courtesy"
        );

        let mut app = app();
        app.insert_resource(ButtonInput::<MouseButton>::default());
        app.update();
        let target = cell_at(&mut app, chest_cell.slot);
        *app.world_mut()
            .entity_mut(target)
            .get_mut::<Interaction>()
            .expect("an equipment cell is a button") = Interaction::Hovered;
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        app.update();

        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<InventoryClick>>()
                .drain()
                .collect::<Vec<_>>(),
            vec![InventoryClick {
                slot: chest_cell.slot,
                kind: InventoryClickKind::Full,
            }],
            "the tint changed or suppressed the existing click"
        );
    }

    /// Shift held over a left press reports a drop; split and consume remain independent.
    ///
    /// One table because the claim is the *pair*: what shift changes is where the stack
    /// goes, so it applies to the button that already means the whole cell and not to the
    /// one that means half. The unheld row keeps the keyboard resource present rather than
    /// absent — a build where shift exists and is simply not down is the case a modifier
    /// wired backwards would pass with the resource missing.
    ///
    /// Nothing here decides whether that cell may be put down: `player::inventory` builds
    /// the frame and the server answers it.
    #[test]
    fn every_mouse_gesture_has_one_meaning_with_or_without_shift() {
        for (grid, slot) in [
            (InventoryGrid::Hotbar, 1),
            (InventoryGrid::Pack, 2),
            (InventoryGrid::Equipment, 3),
        ] {
            for (shift, button, want) in [
                (
                    Some(KeyCode::ShiftLeft),
                    MouseButton::Left,
                    InventoryClickKind::Drop,
                ),
                (
                    Some(KeyCode::ShiftRight),
                    MouseButton::Left,
                    InventoryClickKind::Drop,
                ),
                (
                    Some(KeyCode::ShiftLeft),
                    MouseButton::Right,
                    InventoryClickKind::Split,
                ),
                (
                    Some(KeyCode::ShiftLeft),
                    MouseButton::Middle,
                    InventoryClickKind::Consume,
                ),
                (None, MouseButton::Left, InventoryClickKind::Full),
                (None, MouseButton::Middle, InventoryClickKind::Consume),
            ] {
                let mut app = app();
                app.insert_resource(ButtonInput::<MouseButton>::default());
                app.insert_resource(ButtonInput::<KeyCode>::default());
                app.update();

                let cell = {
                    let world = app.world_mut();
                    let mut query = world.query::<(Entity, &InventoryCell)>();
                    query
                        .iter(world)
                        .find(|(_, cell)| cell.slot == slot && cell.grid == grid)
                        .map(|(entity, _)| entity)
                        .unwrap_or_else(|| panic!("{grid:?} slot {slot} exists"))
                };
                *app.world_mut()
                    .entity_mut(cell)
                    .get_mut::<Interaction>()
                    .expect("buttons carry Interaction") = Interaction::Hovered;
                if let Some(shift) = shift {
                    app.world_mut()
                        .resource_mut::<ButtonInput<KeyCode>>()
                        .press(shift);
                }
                app.world_mut()
                    .resource_mut::<ButtonInput<MouseButton>>()
                    .press(button);
                app.update();

                let clicks: Vec<InventoryClick> = app
                    .world_mut()
                    .resource_mut::<Messages<InventoryClick>>()
                    .drain()
                    .collect();
                assert_eq!(
                    clicks,
                    vec![InventoryClick { slot, kind: want }],
                    "{grid:?}: {shift:?} with {button:?}"
                );
            }
        }
    }

    /// The slot the consume gestures below are aimed at. Any cell will do — whether what
    /// is in it may be consumed is `player::inventory`'s question and not this system's.
    const HOVERED_SLOT: u8 = 0;

    /// Runs one frame of the inventory screen and reports the clicks it wrote.
    ///
    /// `settings` absent means the resource is never inserted, which is the headless build
    /// `inventory_clicks` falls back to [`Bindings::default`] for.
    fn consume_frame(
        settings: Option<Settings>,
        key: Option<KeyCode>,
        button: Option<MouseButton>,
        mode: InputMode,
        hovered: bool,
    ) -> Vec<InventoryClick> {
        let mut app = app();
        app.insert_resource(mode);
        if let Some(settings) = settings {
            app.insert_resource(settings);
        }
        app.insert_resource(ButtonInput::<MouseButton>::default());
        app.insert_resource(ButtonInput::<KeyCode>::default());
        app.update();

        if hovered {
            let cell = cell_at(&mut app, HOVERED_SLOT);
            *app.world_mut()
                .entity_mut(cell)
                .get_mut::<Interaction>()
                .expect("buttons carry Interaction") = Interaction::Hovered;
        }
        if let Some(key) = key {
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(key);
        }
        if let Some(button) = button {
            app.world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .press(button);
        }
        app.update();

        app.world_mut()
            .resource_mut::<Messages<InventoryClick>>()
            .drain()
            .collect()
    }

    /// Settings with the consume control moved off its default.
    fn consume_bound_to(key: KeyCode) -> Settings {
        let mut settings = Settings::default();
        settings
            .rebind(Control::Consume, key)
            .expect("the test picked a free key");
        settings
    }

    /// **The configured key and middle-click are one gesture, and pressing both is one
    /// press.** The last row is the whole point of sharing a branch: two branches would
    /// write two `InventoryClick`s for one intent, and `player::inventory` would build two
    /// `ConsumeRequest` frames out of them.
    #[test]
    fn the_consume_key_and_middle_click_report_one_press_together_or_apart() {
        for (label, settings, key, button) in [
            ("the default key alone", None, Some(KeyCode::KeyC), None),
            ("middle-click alone", None, None, Some(MouseButton::Middle)),
            (
                "both in one frame",
                None,
                Some(KeyCode::KeyC),
                Some(MouseButton::Middle),
            ),
            (
                "the default key with the resource present",
                Some(Settings::default()),
                Some(KeyCode::KeyC),
                None,
            ),
            (
                "a rebound key",
                Some(consume_bound_to(KeyCode::KeyV)),
                Some(KeyCode::KeyV),
                None,
            ),
            (
                "a rebound key beside middle-click",
                Some(consume_bound_to(KeyCode::KeyV)),
                Some(KeyCode::KeyV),
                Some(MouseButton::Middle),
            ),
        ] {
            assert_eq!(
                consume_frame(settings, key, button, InputMode::Inventory, true),
                vec![InventoryClick {
                    slot: HOVERED_SLOT,
                    kind: InventoryClickKind::Consume,
                }],
                "{label}"
            );
        }
    }

    /// Everything that must produce no consume press at all.
    ///
    /// The mode guard and the hover are this system's own; the key that was rebound away is
    /// the one a stale reader of the bindings would get wrong. What is deliberately *not*
    /// here is a non-consumable cell — that is `player::inventory`'s judgement, and asking it
    /// twice is what this module's "nothing is decided here" contract forbids.
    #[test]
    fn the_consume_key_is_silent_in_every_state_that_is_not_a_press_over_a_cell() {
        for (label, settings, key, mode, hovered) in [
            (
                "gameplay owns the input",
                None,
                Some(KeyCode::KeyC),
                InputMode::Playing,
                true,
            ),
            (
                "a menu owns the input",
                None,
                Some(KeyCode::KeyC),
                InputMode::Menu,
                true,
            ),
            (
                "chat owns the keyboard",
                None,
                Some(KeyCode::KeyC),
                InputMode::Chat,
                true,
            ),
            (
                "no cell is hovered",
                None,
                Some(KeyCode::KeyC),
                InputMode::Inventory,
                false,
            ),
            ("nothing is pressed", None, None, InputMode::Inventory, true),
            (
                "the key the control was rebound away from",
                Some(consume_bound_to(KeyCode::KeyV)),
                Some(KeyCode::KeyC),
                InputMode::Inventory,
                true,
            ),
        ] {
            assert_eq!(
                consume_frame(settings, key, None, mode, hovered),
                Vec::new(),
                "{label}"
            );
        }
    }

    /// One keyboard event in the shape winit delivers it.
    ///
    /// Written as a message rather than poked into [`ButtonInput`] because that resource is
    /// `keyboard_input_system`'s to maintain: it clears `just_pressed` at the top of every
    /// frame, so a press set directly reaches `Update` already forgotten. `player/combat.rs`
    /// says the same thing about the mouse half.
    ///
    /// `logical_key` is deliberately unidentified: `keyboard_input_system` presses it into
    /// `ButtonInput<Key>`, which nothing on this path reads, and naming a layout here would
    /// suggest otherwise.
    fn key_event(key: KeyCode, state: ButtonState, repeat: bool) -> KeyboardInput {
        KeyboardInput {
            key_code: key,
            logical_key: Key::Unidentified(NativeKey::Unidentified),
            state,
            text: None,
            repeat,
            window: Entity::PLACEHOLDER,
        }
    }

    /// The inventory screen with the real keyboard pipeline behind it, one cell hovered.
    ///
    /// [`InputPlugin`] is the whole difference from [`consume_frame`]: it puts
    /// `keyboard_input_system` in `PreUpdate`, so `ButtonInput<KeyCode>` is maintained the
    /// way the running client maintains it — cleared each frame, pressed once per `Pressed`
    /// event, and emptied of a key only by a `Released`. A test that set the resource by
    /// hand across frames would be restating that mechanism instead of exercising it.
    fn held_key_app() -> App {
        let mut app = app();
        app.add_plugins(InputPlugin);
        app.update();

        let cell = cell_at(&mut app, HOVERED_SLOT);
        *app.world_mut()
            .entity_mut(cell)
            .get_mut::<Interaction>()
            .expect("buttons carry Interaction") = Interaction::Hovered;
        app
    }

    /// Runs one frame after delivering the events winit would have delivered for it, and
    /// reports what the screen wrote.
    fn keyboard_frame(
        app: &mut App,
        events: impl IntoIterator<Item = KeyboardInput>,
    ) -> Vec<InventoryClick> {
        for event in events {
            app.world_mut().write_message(event);
        }
        app.update();
        app.world_mut()
            .resource_mut::<Messages<InventoryClick>>()
            .drain()
            .collect()
    }

    fn consume_press() -> InventoryClick {
        InventoryClick {
            slot: HOVERED_SLOT,
            kind: InventoryClickKind::Consume,
        }
    }

    /// **A held consume key is one press, measured rather than read out of a dependency.**
    ///
    /// This exists to answer the review finding on PR #403, which argued that OS key repeat
    /// would re-arm `just_pressed` every frame and drain a food stack automatically. The
    /// finding was refuted and the thread resolved, but nothing in this tree pinned the
    /// refutation: half of it is right — `keyboard_input_system` does not filter
    /// `KeyboardInput { repeat: true }` — and what saves the other half lives in
    /// `bevy_input`, whose `press()` arms `just_pressed` only when `pressed.insert()`
    /// reports the key was not already down. That is a **dependency's** guarantee, so a Bevy
    /// upgrade could take it away with nothing here turning red. Do not delete this as
    /// redundant with the single-frame cases above: they press for one frame and would
    /// notice nothing.
    ///
    /// The cell under the pointer is [`HOVERED_SLOT`], which is occupied and nothing more:
    /// whether what is in it may be consumed is `player::inventory`'s question, and asking it
    /// here is what this module's "nothing is decided here" contract forbids. What a
    /// repeat would have drained is a stack, and this is the press that would have drained
    /// it.
    #[test]
    fn holding_the_consume_key_reports_one_press_and_a_later_press_reports_again() {
        const KEY: KeyCode = KeyCode::KeyC;
        let mut app = held_key_app();
        let mut reported = Vec::new();

        reported.extend(keyboard_frame(
            &mut app,
            [key_event(KEY, ButtonState::Pressed, false)],
        ));
        // Three more frames of the key being held: the repeats winit sends when the OS is
        // configured to send them, then a frame carrying no event at all, which is what the
        // same held key looks like when it is not.
        for _ in 0..3 {
            reported.extend(keyboard_frame(
                &mut app,
                [key_event(KEY, ButtonState::Pressed, true)],
            ));
            assert!(
                app.world().resource::<ButtonInput<KeyCode>>().pressed(KEY),
                "a repeat is not a release"
            );
        }
        reported.extend(keyboard_frame(&mut app, []));
        assert!(
            app.world().resource::<ButtonInput<KeyCode>>().pressed(KEY),
            "a silent frame is not a release either"
        );
        assert_eq!(
            reported,
            vec![consume_press()],
            "a held key is one press, so one stack is not consumed frame by frame"
        );

        // Releasing and pressing again has to report a second time. A test that only proved
        // "one" would pass just as well if the key had stopped working entirely.
        reported.extend(keyboard_frame(
            &mut app,
            [key_event(KEY, ButtonState::Released, false)],
        ));
        assert!(
            !app.world().resource::<ButtonInput<KeyCode>>().pressed(KEY),
            "the key was let go"
        );
        reported.extend(keyboard_frame(
            &mut app,
            [key_event(KEY, ButtonState::Pressed, false)],
        ));
        assert_eq!(
            reported,
            vec![consume_press(), consume_press()],
            "a press after a release is a second press"
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

    fn filter_button(app: &mut App, filter: CraftFilter) -> Entity {
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &CraftFilterButton)>();
        query
            .iter(world)
            .find(|(_, button)| button.0 == filter)
            .map(|(entity, _)| entity)
            .unwrap_or_else(|| panic!("{filter:?} has a button"))
    }

    fn visible_recipes(app: &mut App) -> Vec<RecipeId> {
        let world = app.world_mut();
        let mut query = world.query::<(&CraftRow, &Node)>();
        query
            .iter(world)
            .filter(|(_, node)| node.display != Display::None)
            .map(|(row, _)| row.0.id)
            .collect()
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
    fn the_complete_recipe_list_is_bounded_by_a_scroll_viewport_with_a_visible_bar() {
        let mut app = app();
        app.update();

        let world = app.world_mut();
        let mut areas = world.query_filtered::<(&Node, &ScrollPosition), With<CraftScrollArea>>();
        let (area, position) = areas.single(world).expect("one crafting scroll area");
        assert_eq!(area.overflow.y, OverflowAxis::Scroll);
        assert_eq!(area.flex_grow, 1.0);
        assert_eq!(area.min_height, Val::Px(0.0));
        assert_eq!(position.0, Vec2::ZERO);

        let mut rails = world.query_filtered::<Entity, With<CraftScrollbar>>();
        let mut thumbs = world.query_filtered::<Entity, With<CraftScrollbarThumb>>();
        assert_eq!(rails.iter(world).count(), 1, "the scroll area has no rail");
        assert_eq!(
            thumbs.iter(world).count(),
            1,
            "the scroll area has no thumb"
        );

        let mut panels = world.query::<(&TabPanel, &Node)>();
        let crafting = panels
            .iter(world)
            .find(|(panel, _)| panel.0 == InventoryTab::Crafting)
            .map(|(_, node)| node)
            .expect("the crafting tab has a panel");
        assert_eq!(crafting.height, Val::Percent(100.0));
    }

    #[test]
    fn recipe_filters_partition_the_same_rows_without_originating_new_ones() {
        let mut app = app();
        app.update();

        assert_eq!(*app.world().resource::<CraftFilter>(), CraftFilter::All);
        assert_eq!(visible_recipes(&mut app).len(), RECIPES.len());

        let crafting = tab_button(&mut app, InventoryTab::Crafting);
        *app.world_mut()
            .entity_mut(crafting)
            .get_mut::<Interaction>()
            .expect("the crafting tab is a button") = Interaction::Pressed;
        app.update();

        for (filter, category) in [
            (CraftFilter::Survival, RecipeCategory::Survival),
            (CraftFilter::Tools, RecipeCategory::Tools),
            (CraftFilter::Armour, RecipeCategory::Armour),
        ] {
            for candidate in CraftFilter::ALL {
                let button = filter_button(&mut app, candidate);
                *app.world_mut()
                    .entity_mut(button)
                    .get_mut::<Interaction>()
                    .expect("a filter is a button") = if candidate == filter {
                    Interaction::Pressed
                } else {
                    Interaction::None
                };
            }
            app.update();

            let visible = visible_recipes(&mut app);
            let expected: Vec<RecipeId> = RECIPES
                .iter()
                .filter(|recipe| recipe.category == category)
                .map(|recipe| recipe.id)
                .collect();
            assert_eq!(visible, expected, "{filter:?} showed another shelf");
        }

        let world = app.world_mut();
        let mut rows = world.query::<&CraftRow>();
        assert_eq!(
            rows.iter(world).count(),
            RECIPES.len(),
            "filtering added or removed mirrored rows"
        );
    }

    #[test]
    fn recipe_scroll_moves_by_input_unit_and_clamps_to_the_content() {
        assert_eq!(
            scrolled_offset(0.0, -1.0, MouseScrollUnit::Line, 200.0, 500.0),
            CRAFT_SCROLL_LINE
        );
        assert_eq!(
            scrolled_offset(48.0, 12.0, MouseScrollUnit::Pixel, 200.0, 500.0),
            36.0
        );
        assert_eq!(
            scrolled_offset(290.0, -10.0, MouseScrollUnit::Line, 200.0, 500.0),
            300.0
        );
        assert_eq!(
            scrolled_offset(50.0, -1.0, MouseScrollUnit::Line, 300.0, 200.0),
            0.0,
            "a shelf shorter than its viewport still scrolled"
        );

        let (thumb, top) = scrollbar_geometry(150.0, 200.0, 500.0, 100.0);
        assert_eq!(thumb, 40.0);
        assert_eq!(top, 30.0);
    }

    #[test]
    fn a_layout_change_reclamps_scroll_without_another_wheel_event() {
        let mut app = app();
        app.update();

        let (area, rail, thumb) = {
            let world = app.world_mut();
            let mut areas = world.query_filtered::<Entity, With<CraftScrollArea>>();
            let mut rails = world.query_filtered::<Entity, With<CraftScrollbar>>();
            let mut thumbs = world.query_filtered::<Entity, With<CraftScrollbarThumb>>();
            (
                areas.single(world).expect("one crafting scroll area"),
                rails.single(world).expect("one crafting scrollbar"),
                thumbs.single(world).expect("one crafting scrollbar thumb"),
            )
        };
        app.world_mut().entity_mut(area).insert((
            ScrollPosition(Vec2::new(0.0, 250.0)),
            ComputedNode {
                size: Vec2::new(600.0, 300.0),
                content_size: Vec2::new(600.0, 400.0),
                inverse_scale_factor: 1.0,
                ..ComputedNode::DEFAULT
            },
        ));
        app.world_mut().entity_mut(rail).insert(ComputedNode {
            size: Vec2::new(CRAFT_SCROLLBAR_WIDTH, 100.0),
            inverse_scale_factor: 1.0,
            ..ComputedNode::DEFAULT
        });

        app.world_mut()
            .run_system_once(sync_craft_scrollbar)
            .expect("the post-layout scrollbar system runs");

        assert_eq!(
            app.world().get::<ScrollPosition>(area).unwrap().y,
            100.0,
            "the offset stayed beyond the shortened content"
        );
        let thumb = app.world().get::<Node>(thumb).unwrap();
        assert_eq!(thumb.height, Val::Px(75.0));
        assert_eq!(thumb.top, Val::Px(25.0));
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
            BUTTON,
            "the eighth log did not enable the row"
        );

        // Counted across slots, exactly as the server spends across slots.
        deliver(&mut app, &[(LOG, 5), (LOG, 3)]);
        assert_eq!(row_colour(&mut app, RecipeId::Tent), BUTTON);
    }

    /// Proximity is the server's call, so a station recipe is never grayed out for want of
    /// its station — only for want of materials, and it says what else it needs in its note.
    #[test]
    fn a_station_recipe_with_the_materials_is_enabled_and_labelled() {
        let mut app = app();
        deliver(&mut app, &[(STONE, 2), (COAL, 1)]);

        assert_eq!(
            row_colour(&mut app, RecipeId::SharpeningStone),
            BUTTON,
            "a recipe this client cannot know the station for was drawn as unavailable"
        );

        let world = app.world_mut();
        let mut query = world.query::<(&Text, &TextColor)>();
        let mut notes: Vec<String> = query
            .iter(world)
            .filter(|(_, colour)| colour.0 == RECIPE_STATION)
            .map(|(text, _)| text.0.clone())
            .collect();
        // Counted off the mirror rather than spelled as a literal. It was two identical
        // strings, and #185 made it five — a number this test had no opinion about and was
        // asserting anyway. What it is actually for is that a station note appears exactly
        // where a station is required, and that survives a sixth recipe.
        let mut expected: Vec<String> = RECIPES
            .iter()
            .filter_map(|recipe| recipe.station.map(station_note))
            .collect();
        notes.sort();
        expected.sort();
        assert_eq!(
            notes, expected,
            "station notes do not mirror the recipe table"
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

    /// The readout uses the purse without manufacturing an inventory stack for it.
    ///
    /// Forty-one beside seven visually empty cells proves the number came from
    /// `InventoryState.silver`, not from a slot or a client-side reconstruction.
    #[test]
    fn the_authoritative_purse_does_not_render_as_an_inventory_stack() {
        let mut app = app();
        *app.world_mut().resource_mut::<Inventory>() = pursed_inventory();
        app.update();

        assert_eq!(silver_label(&mut app), "41");
        for slot in 0..7 {
            let cell = drawn(&mut app, slot);
            assert_eq!(cell.count, "", "slot {slot} drew a currency count");
            assert!(
                cell.rectangles.is_empty(),
                "slot {slot} drew a currency icon"
            );
        }
    }

    /// A pack with no silver in it reads `0` rather than reading nothing.
    ///
    /// An absent count would be indistinguishable from a readout that had stopped
    /// refreshing, which is the failure this screen has no other way to show.
    #[test]
    fn the_readout_reads_zero_when_there_is_no_silver() {
        let mut app = app();
        app.update();

        assert_eq!(silver_label(&mut app), "0");

        // And it follows the state rather than being written once at spawn.
        *app.world_mut().resource_mut::<Inventory>() = pursed_inventory();
        app.update();
        assert_eq!(silver_label(&mut app), "41");
    }

    /// The readout belongs to the window, so both tabs show it and neither owns it.
    ///
    /// `Display` is what selects a tab — a `TabPanel` that is not active is laid out at no
    /// size — so a readout parented to one would vanish with it. Asserting the parent is
    /// stronger than asserting it is visible on both: it is the reason it is visible on both.
    ///
    /// `FocusPolicy::Pass` is the other half. The readout is absolutely positioned over the
    /// bottom of whichever panel is showing, and a node with no policy **blocks**, so
    /// without this a strip of that panel would stop responding to the pointer with nothing
    /// on screen to explain it.
    #[test]
    fn the_readout_hangs_off_the_window_and_passes_the_pointer() {
        let mut app = app();
        app.update();

        let window = inventory_window(&mut app);
        let readout = silver_readout(&mut app);
        let parent = app
            .world()
            .get::<ChildOf>(readout)
            .expect("the readout is parented");
        assert_eq!(
            parent.parent(),
            window,
            "the readout hangs off a tab panel rather than off the window"
        );
        assert_eq!(
            app.world().get::<FocusPolicy>(readout),
            Some(&FocusPolicy::Pass)
        );

        // Both tabs, read the way the panels themselves are selected: the readout is not one
        // of them and is never the node a switch touches.
        for tab in InventoryTab::ALL {
            *app.world_mut().resource_mut::<InventoryTab>() = tab;
            app.update();
            assert!(
                !shown(&mut app).is_empty(),
                "no panel is showing, so {tab:?} asked nothing"
            );
            let node = app
                .world()
                .get::<Node>(readout)
                .expect("the readout's node");
            assert_ne!(
                node.display,
                Display::None,
                "the readout is hidden on {tab:?}"
            );
        }
    }

    /// The currency icon still comes from the client display row reserved for that purpose.
    ///
    /// The server item registry has a reserved hole at 35, while this client keeps one
    /// presentation row for the coin used by purse and corpse currency surfaces.
    #[test]
    fn the_purse_coin_uses_the_reserved_display_row() {
        let coin = silver_icon();
        assert_eq!(coin.shape, ItemShape::Coin);
        assert_eq!(coin.livery, None);
        let [r, g, b, a] = crate::player::item_linear_rgba(ITEM_SILVER);
        assert_eq!(coin.colour, Color::linear_rgba(r, g, b, a));
    }
}
