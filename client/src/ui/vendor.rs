//! Dedicated view of one complete server-owned price list.
//!
//! **`ui/loot.rs`'s window with two columns in it**, and it decides as little as that one
//! does: the rows are the two vectors of the newest [`VendorState`] laid out, the prices
//! are the server's, and nothing here computes a total or a balance. The one number this
//! side contributes is how much of a thing the player is carrying, which comes from the
//! last authoritative inventory the server sent and is the reason the sell column can show
//! what a player has to offer at all.

use bevy::prelude::*;

use super::inventory::{refresh_silver_readout, spawn_silver_readout};
use super::{FILLED_CELL, icon, stack_style};
use crate::net::{InventoryStack, Session, VendorEntry};
use crate::player::{InputMode, Inventory, Liveries, VendorWindow, item_label};

const WIDTH: f32 = 620.0;

/// The frame's own padding. The bottom is deeper than the other three because the purse in
/// the corner is absolutely positioned over the content, and a row underneath it would be
/// a price with a coin sitting on top of it.
const PADDING: f32 = 16.0;
const BOTTOM_PADDING: f32 = 40.0;

/// Smaller than a pack cell, deliberately: two columns of up to seven rows have to fit one
/// window, and a stall row is a picture beside a sentence rather than a square a player
/// drags things out of.
const ROW_ICON: f32 = 34.0;

/// Dimmer than the rows, for the reason the loot window's hint line is: an empty-column
/// line is a label rather than something to read a price off.
const HINT: Color = Color::srgb(0.62, 0.62, 0.66);

/// What a column with no rows says. **Neither is a refusal**, and the wording is chosen so
/// it cannot read as one: the vendor's own list is what has nothing in it, and the sell
/// column is empty exactly when the pack holds none of what this vendor takes.
const NOTHING_FOR_SALE: &str = "nothing for sale here";
const NOTHING_WANTED: &str = "nothing here they will buy";

/// One drawn line: the vendor's entry, and what the player holds of it when that is
/// something the column shows.
type Row = (VendorEntry, Option<u32>);

#[derive(Component)]
struct VendorRoot;

pub(super) struct VendorUiPlugin;

impl Plugin for VendorUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VendorWindow>()
            .init_resource::<InputMode>()
            .add_systems(Startup, spawn_window)
            // `refresh_silver_readout` belongs to `ui/inventory.rs` and is registered by
            // that plugin too. One function and two registrations rather than a second
            // purse: it is a query and a string comparison, it writes the same label
            // whichever pass reaches a given readout first, and the alternative is a
            // number in this window that can disagree with the number in the pack.
            // Chained through `ApplyDeferred` for the reason `ui/inventory.rs`'s systems
            // are: the readout is spawned by the rebuild's deferred commands, and a
            // refresh scheduled before they are applied would find no readout to write
            // and leave the purse reading zero for a frame.
            .add_systems(
                Update,
                (
                    rebuild_window,
                    ApplyDeferred,
                    refresh_silver_readout,
                    show_window,
                )
                    .chain(),
            );
    }
}

fn spawn_window(mut commands: Commands) {
    commands.spawn((
        VendorRoot,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(WIDTH),
            max_height: Val::Percent(80.0),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
            padding: UiRect {
                left: Val::Px(PADDING),
                right: Val::Px(PADDING),
                top: Val::Px(PADDING),
                bottom: Val::Px(BOTTOM_PADDING),
            },
            overflow: Overflow::scroll_y(),
            ..default()
        },
        UiTransform::from_translation(Val2::percent(-50.0, -50.0)),
        BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.96)),
        GlobalZIndex(30),
        Visibility::Hidden,
    ));
}

/// Rebuilds both columns from the newest list and the newest pack.
///
/// **Two change signals, because the window shows two authoritative things.** A
/// `VendorState` replaces the prices; an `InventoryState` changes which of the vendor's
/// buy rows the player has anything to fill, and a sell column that only rebuilt with the
/// price list would keep offering a stack that has since been spent.
fn rebuild_window(
    window: Res<VendorWindow>,
    inventory: Option<Res<Inventory>>,
    roots: Query<Entity, With<VendorRoot>>,
    mut commands: Commands,
    // Optional, because the UI stands up headlessly without the player plugin that owns it.
    liveries: Option<Res<Liveries>>,
) {
    let inventory_moved = inventory.as_ref().is_some_and(DetectChanges::is_changed);
    if !window.is_changed() && !inventory_moved {
        return;
    }
    for root in &roots {
        commands.entity(root).despawn_related::<Children>();
        let Some(state) = window.state() else {
            continue;
        };
        commands.entity(root).with_children(|root| {
            root.spawn((
                Text::new(format!("Trade | revision {}", state.revision)),
                TextFont {
                    font_size: FontSize::Px(22.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            // What the player may buy is every row the vendor sells, whether or not the
            // purse covers it: whether it does is the server's answer, and a list pruned
            // here would be this client refusing a trade it was never asked about.
            let buy: Vec<Row> = state.sells.iter().map(|entry| (*entry, None)).collect();
            // What the vendor buys, narrowed to what the player is actually carrying: a
            // row for a thing nobody holds is an offer that cannot be taken, and the count
            // beside it is the last complete state the server sent rather than a running
            // total kept on this side.
            let sell: Vec<Row> = state
                .buys
                .iter()
                .filter_map(|entry| {
                    held(inventory.as_deref(), entry.item_id).map(|held| (*entry, Some(held)))
                })
                .collect();
            root.spawn(Node {
                width: Val::Percent(100.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(16.0),
                ..default()
            })
            .with_children(|columns| {
                spawn_column(columns, "Buy", &buy, NOTHING_FOR_SALE, liveries.as_deref());
                spawn_column(columns, "Sell", &sell, NOTHING_WANTED, liveries.as_deref());
            });
            spawn_silver_readout(root, PADDING);
        });
    }
}

/// How many of one item the last authoritative inventory holds, or `None` for none at all.
///
/// **Every slot, which is the total and not a second opinion about where money may sit.**
/// No row of the server's table names a wearable — the four buy lists are raw iron, coal,
/// logs, meat, bone, pelts and patches — so the count the server spends from the pack and
/// the count this readout sums are the same number for every row that can appear here.
fn held(inventory: Option<&Inventory>, item_id: u16) -> Option<u32> {
    inventory
        .map(|inventory| inventory.count(item_id))
        .filter(|count| *count > 0)
}

/// One column: a heading, then its rows, or one dim line when it has none.
fn spawn_column(
    columns: &mut ChildSpawnerCommands<'_>,
    heading: &str,
    rows: &[Row],
    when_empty: &str,
    liveries: Option<&Liveries>,
) {
    columns
        .spawn(Node {
            // A basis of zero with equal growth, so the two columns are halves of the
            // window rather than halves of whichever list is longer.
            flex_basis: Val::Px(0.0),
            flex_grow: 1.0,
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(6.0),
            ..default()
        })
        .with_children(|column| {
            column.spawn((
                Text::new(heading.to_owned()),
                TextFont {
                    font_size: FontSize::Px(17.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            for (entry, held) in rows {
                spawn_row(column, *entry, *held, liveries);
            }
            if rows.is_empty() {
                column.spawn((
                    Text::new(when_empty.to_owned()),
                    TextFont {
                        font_size: FontSize::Px(14.0),
                        ..default()
                    },
                    TextColor(HINT),
                ));
            }
        });
}

/// One row: the icon, the label, what the player holds of it, and the silver per unit.
///
/// ASCII throughout, and the separator is the pipe the rest of this client uses. Bevy's
/// embedded fallback font is the whole font stack here — a 95-glyph subset — so a
/// typographic dash on a price would draw as nothing at all.
fn spawn_row(
    column: &mut ChildSpawnerCommands<'_>,
    entry: VendorEntry,
    held: Option<u32>,
    liveries: Option<&Liveries>,
) {
    let style = stack_style(Some(InventoryStack {
        item_id: entry.item_id,
        count: 1,
        ..Default::default()
    }));
    let holding = match held {
        Some(held) => format!(" x {held}"),
        None => String::new(),
    };
    column
        .spawn(Node {
            width: Val::Percent(100.0),
            min_height: Val::Px(ROW_ICON + 8.0),
            display: Display::Flex,
            align_items: AlignItems::Center,
            column_gap: Val::Px(8.0),
            padding: UiRect::all(Val::Px(4.0)),
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Node {
                    width: Val::Px(ROW_ICON),
                    height: Val::Px(ROW_ICON),
                    flex_shrink: 0.0,
                    position_type: PositionType::Relative,
                    ..default()
                },
                BackgroundColor(FILLED_CELL),
            ))
            .with_children(|host| {
                if let Some(icon) = style.icon {
                    icon::spawn(host, icon, liveries);
                }
            });
            row.spawn((
                Text::new(format!(
                    "{}{holding} | {} silver",
                    item_label(entry.item_id),
                    entry.price
                )),
                TextFont {
                    font_size: FontSize::Px(15.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

fn show_window(
    window: Res<VendorWindow>,
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Visibility, With<VendorRoot>>,
) {
    let shown = session.is_some() && *mode == InputMode::Vendor && window.state().is_some();
    for mut visibility in &mut roots {
        *visibility = if shown {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, SessionParams, VendorState};
    use crate::player::ITEM_SILVER;
    use crate::ui::inventory::SilverCount;

    /// Three ids from the smith's list in `server/internal/game/vendor.go`. Spelled as
    /// numbers because the registry's own constants are private to `player::items`, and
    /// every assertion below reads the label back through [`item_label`] rather than
    /// hard-coding a name — so a wrong number here fails rather than passing quietly.
    const PICKAXE: u16 = 17;
    const RAW_IRON: u16 = 6;
    const RAW_COAL: u16 = 5;

    fn session() -> Session {
        Session(SessionParams {
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
        })
    }

    /// A smith's list, trimmed to one row each way plus a second buy row the player is
    /// holding none of.
    fn smith() -> VendorState {
        VendorState {
            entity_id: (1 << 62) | 55,
            revision: 3,
            sells: vec![VendorEntry {
                item_id: PICKAXE,
                price: 25,
            }],
            buys: vec![
                VendorEntry {
                    item_id: RAW_IRON,
                    price: 3,
                },
                VendorEntry {
                    item_id: RAW_COAL,
                    price: 1,
                },
            ],
        }
    }

    /// One authoritative slot, as the server sends them.
    fn stack(item_id: u16, count: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count,
            ..Default::default()
        }
    }

    /// The inventory this test reads its held counts from: four raw iron and twelve
    /// silver, and no coal at all.
    fn carrying() -> Inventory {
        Inventory::from_stacks(vec![stack(RAW_IRON, 4), stack(ITEM_SILVER, 12)])
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(InputMode::Vendor)
            .insert_resource(VendorWindow::from_server(smith()))
            .insert_resource(carrying())
            .add_plugins(VendorUiPlugin);
        app.update();
        app
    }

    fn lines(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        let mut texts = world.query::<&Text>();
        texts.iter(world).map(|text| text.0.clone()).collect()
    }

    /// The window draws the vendor's prices, the player's holdings and their purse, and it
    /// draws nothing about a row they have nothing to fill.
    #[test]
    fn both_columns_are_drawn_from_the_list_and_the_pack() {
        let mut app = app();
        let world = app.world_mut();
        let mut roots = world.query_filtered::<&Visibility, With<VendorRoot>>();
        assert_eq!(roots.single(world).unwrap(), &Visibility::Visible);

        let drawn = lines(&mut app);
        assert!(
            drawn.contains(&"Trade | revision 3".to_owned()),
            "{drawn:?}"
        );
        assert!(drawn.contains(&"Buy".to_owned()) && drawn.contains(&"Sell".to_owned()));
        assert!(
            drawn.contains(&format!("{} | 25 silver", item_label(PICKAXE))),
            "{drawn:?}"
        );
        assert!(
            drawn.contains(&format!("{} x 4 | 3 silver", item_label(RAW_IRON))),
            "{drawn:?}"
        );
        assert!(
            !drawn.iter().any(|line| line.contains(item_label(RAW_COAL))),
            "the sell column offered a row the player holds none of: {drawn:?}"
        );

        // The purse, through the readout `ui/inventory.rs` owns rather than a second one.
        let world = app.world_mut();
        let mut counts = world.query_filtered::<&Text, With<SilverCount>>();
        assert_eq!(counts.single(world).unwrap().0, "12");

        for line in lines(&mut app) {
            assert!(line.is_ascii(), "{line}");
        }
    }

    /// **Spending the last of a stack takes its row out of the sell column**, which is the
    /// half of "rows rebuild" that a `VendorState` alone would never have exercised: the
    /// price list did not move, the pack did.
    #[test]
    fn a_pack_that_moved_rebuilds_the_sell_column() {
        let mut app = app();
        app.insert_resource(Inventory::from_stacks(vec![stack(ITEM_SILVER, 12)]));
        app.update();

        let drawn = lines(&mut app);
        assert!(
            !drawn.iter().any(|line| line.contains(item_label(RAW_IRON))),
            "a row survived the stack it was drawn from being spent: {drawn:?}"
        );
        assert!(
            drawn.contains(&"nothing here they will buy".to_owned()),
            "{drawn:?}"
        );
        assert!(
            drawn.contains(&format!("{} | 25 silver", item_label(PICKAXE))),
            "the buy column is not the player's to fill and went with it: {drawn:?}"
        );
    }

    /// Leaving the mode hides the window; the list is still what the server last said.
    #[test]
    fn the_window_is_hidden_outside_its_own_mode() {
        let mut app = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();

        let world = app.world_mut();
        let mut roots = world.query_filtered::<&Visibility, With<VendorRoot>>();
        assert_eq!(roots.single(world).unwrap(), &Visibility::Hidden);
    }
}
