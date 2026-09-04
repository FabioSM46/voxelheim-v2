//! The always-visible row of server-announced hotbar slots.
//!
//! A cell draws the item's picture, not a rectangle of its colour — see [`super::icon`] —
//! and the drawing is the one `super::inventory` shows for the same slot, because both go
//! through [`stack_style`].

use bevy::prelude::*;

use super::icon::DrawnIcon;
use super::{
    CELL_EDGE, SELECTED_EDGE, SlotCount, cell_node, refresh_cell_contents, spawn_cell_contents,
    stack_style,
};
use crate::net::Session;
use crate::player::{ApplyInventory, InputMode, Inventory, Liveries, SelectedSlot};

pub(super) struct HotbarPlugin;

impl Plugin for HotbarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_hotbar_root).add_systems(
            Update,
            (build_hotbar, ApplyDeferred, refresh_hotbar, show_hotbar)
                .chain()
                .after(ApplyInventory),
        );
    }
}

#[derive(Component)]
struct HotbarRoot;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct HotbarCell(u8);

fn spawn_hotbar_root(mut commands: Commands) {
    commands.spawn((
        HotbarRoot,
        hotbar_root_node(),
        Visibility::Hidden,
        GlobalZIndex(12),
    ));
}

/// The viewport-spanning contract that keeps the hotbar on the horizontal axis.
pub(super) fn hotbar_root_node() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(0.0),
        right: Val::Px(0.0),
        bottom: Val::Px(18.0),
        display: Display::Flex,
        column_gap: Val::Px(6.0),
        justify_content: JustifyContent::Center,
        ..default()
    }
}

fn build_hotbar(
    mut commands: Commands,
    session: Option<Res<Session>>,
    roots: Query<Entity, With<HotbarRoot>>,
    cells: Query<(Entity, &HotbarCell)>,
) {
    let (Some(session), Some(root)) = (session, roots.iter().next()) else {
        return;
    };
    let expected = usize::from(session.0.hotbar_slots);
    if cells.iter().count() == expected {
        return;
    }

    for (entity, _) in &cells {
        commands.entity(entity).despawn();
    }
    for slot in 0..session.0.hotbar_slots {
        commands
            .spawn((
                HotbarCell(slot),
                ChildOf(root),
                Button,
                cell_node(),
                BackgroundColor(super::EMPTY_CELL),
                BorderColor::all(CELL_EDGE),
            ))
            // The picture and the count are the same two children the pack's cells get,
            // spawned by the same function: one cell, drawn in two places.
            .with_children(spawn_cell_contents);
    }
}

/// Redraws every hotbar cell against the newest authoritative slot.
///
/// The plate and the selection edge belong to this row; what goes *inside* the cell is
/// [`refresh_cell_contents`], shared with the pack. `Without<SlotCount>` on the cells is
/// what keeps the two `BackgroundColor` queries disjoint — the cell's plate and the count's
/// plate are the same component on two entities.
fn refresh_hotbar(
    mut commands: Commands,
    inventory: Option<Res<Inventory>>,
    selected: Option<Res<SelectedSlot>>,
    mut cells: Query<
        (
            &HotbarCell,
            &Children,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        Without<SlotCount>,
    >,
    mut counts: Query<(&mut Text, &mut BackgroundColor), With<SlotCount>>,
    mut icons: Query<&mut DrawnIcon>,
    // Optional, because the UI stands up headlessly without the player plugin that owns it.
    liveries: Option<Res<Liveries>>,
) {
    let (Some(inventory), Some(selected)) = (inventory, selected) else {
        return;
    };

    for (cell, children, mut background, mut border) in &mut cells {
        let style = stack_style(inventory.slot(cell.0));
        if background.0 != style.background {
            background.0 = style.background;
        }
        let edge = if selected.0 == cell.0 {
            SELECTED_EDGE
        } else {
            CELL_EDGE
        };
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
    }
}

fn show_hotbar(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Visibility, With<HotbarRoot>>,
) {
    let next = if *mode == InputMode::Playing && session.is_some() {
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

#[cfg(test)]
mod tests {
    use super::super::{COUNT_PLATE, DrawnCell, drawn_cell, icon};
    use super::*;
    use crate::net::{InventoryStack, SessionParams};
    use crate::player::{ItemShape, item_shape};

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 5,
            hotbar_slots: 3,
            equipment_slots: 1,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    /// Stone, an empty slot, and an id from a newer contract — with the empty one
    /// selected, so the selection edge is asserted where there is no item to confuse it
    /// with.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(Inventory::from_stacks(vec![
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
            ]))
            .insert_resource(SelectedSlot(1))
            .init_resource::<InputMode>()
            .add_plugins(HotbarPlugin);
        app
    }

    #[test]
    fn every_announced_slot_is_drawn_and_the_selection_is_distinct() {
        let mut app = app();
        app.update();

        let cells = cells(&mut app);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].1.count, "5");
        assert_eq!(cells[1].1.count, "");
        assert_eq!(cells[2].1.count, "2");
        assert_eq!(cells[1].2, BorderColor::all(SELECTED_EDGE));
        assert_eq!(cells[0].2, BorderColor::all(CELL_EDGE));
    }

    #[test]
    fn chat_hides_the_hotbar_without_removing_its_cells() {
        let mut app = app();
        app.update();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();

        let world = app.world_mut();
        let mut root = world.query_filtered::<&Visibility, With<HotbarRoot>>();
        assert_eq!(
            *root.single(world).expect("one hotbar root"),
            Visibility::Hidden
        );
        let mut cells = world.query_filtered::<Entity, With<HotbarCell>>();
        assert_eq!(cells.iter(world).count(), 3);
    }

    /// Every slot with something in it draws that thing; the empty one draws nothing.
    ///
    /// The same assertion the pack makes, on the row a player looks at while playing —
    /// the two grids share [`refresh_cell_contents`], and this is what says so from the
    /// hotbar's side.
    #[test]
    fn a_filled_hotbar_cell_draws_its_item_and_an_empty_one_draws_nothing() {
        let mut app = app();
        app.update();

        let cells = cells(&mut app);
        assert_eq!(
            cells[0].1.rectangles.len(),
            icon::parts(item_shape(1)).len(),
            "the stone slot did not draw a block"
        );
        assert!(
            cells[1].1.rectangles.is_empty(),
            "the empty slot drew a picture"
        );
        assert_eq!(
            cells[2].1.rectangles.len(),
            icon::parts(ItemShape::Material).len(),
            "an item from a newer contract drew nothing at all"
        );

        // And the filled cells are a plate for the picture rather than the item's colour
        // spread flat, which is the whole of what this issue changed.
        assert_eq!(cells[0].1.plate, COUNT_PLATE);
        assert_eq!(
            cells[1].1.plate,
            Color::NONE,
            "an empty cell drew a plate for a count it does not have"
        );
    }

    /// One cell per slot, with what each of them drew.
    fn cells(app: &mut App) -> Vec<(u8, DrawnCell, BorderColor)> {
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &HotbarCell, &BorderColor)>();
        let found: Vec<(Entity, u8, BorderColor)> = query
            .iter(world)
            .map(|(entity, cell, border)| (entity, cell.0, *border))
            .collect();
        let mut cells: Vec<(u8, DrawnCell, BorderColor)> = found
            .into_iter()
            .map(|(entity, slot, border)| (slot, drawn_cell(world, entity), border))
            .collect();
        cells.sort_by_key(|(slot, ..)| *slot);
        cells
    }
}
