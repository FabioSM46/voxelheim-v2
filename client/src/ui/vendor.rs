//! Dedicated view of one complete server-owned price list.
//!
//! **`ui/loot.rs`'s window with two columns in it**, and it decides as little as that one
//! does: the rows are the two vectors of the newest [`VendorState`] laid out, the prices are
//! the server's, and nothing here computes a total or a balance. The one number this side
//! contributes is how much of a thing the player is carrying, taken from the last
//! authoritative inventory the server sent.

use bevy::prelude::*;

use super::inventory::{refresh_silver_readout, spawn_silver_readout};
use super::{BUTTON, FILLED_CELL, button_colour, icon, stack_style};
use crate::net::{InventoryStack, Session, VendorEntry};
use crate::player::{
    Appearances, InputMode, Inventory, Liveries, SHIFT_COUNT, SelfVitals, VendorTradeClick,
    VendorWindow, item_label,
};

const WIDTH: f32 = 620.0;

/// The frame's own padding. The bottom is deeper because the purse in the corner is
/// absolutely positioned over the content, and a row underneath it would be a price with a
/// coin sitting on top of it.
const PADDING: f32 = 16.0;
const BOTTOM_PADDING: f32 = 40.0;

/// Smaller than a pack cell, deliberately: two columns of up to seven rows have to fit one
/// window, and a stall row is a picture beside a sentence rather than a square to drag from.
const ROW_ICON: f32 = 34.0;

/// Dimmer than the rows, for the reason the loot window's hint line is: an empty-column
/// line is a label rather than something to read a price off.
const HINT: Color = Color::srgb(0.62, 0.62, 0.66);

/// What a column with no rows says. **Neither is a refusal**, and the wording is chosen so
/// it cannot read as one: the sell column is empty exactly when the pack holds none of what
/// this vendor takes.
const NOTHING_FOR_SALE: &str = "nothing for sale here";
const NOTHING_WANTED: &str = "nothing here they will buy";

/// The two column headings, and the two words on the buttons under them. One pair of
/// constants rather than four literals, because the heading is also what tells
/// [`spawn_column`] which direction its rows trade in.
const BUY: &str = "Buy";
const SELL: &str = "Sell";

/// One drawn line: the vendor's entry, and what the player holds of it when that is
/// something the column shows.
type Row = (VendorEntry, Option<u32>);

#[derive(Component)]
struct VendorRoot;

/// The button on one row, carrying the whole of what pressing it asks for.
///
/// The direction lives here rather than being inferred from which column the button was
/// found in: an item may be in both of a vendor's vectors at different prices — the
/// trader's leather patch is — and a press that only named the item would be ambiguous
/// about which of the two it meant.
#[derive(Component, Debug, Clone, Copy)]
struct VendorRowButton {
    item_id: u16,
    buying: bool,
}

pub(super) struct VendorUiPlugin;

impl Plugin for VendorUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VendorWindow>()
            .init_resource::<InputMode>()
            .add_systems(Startup, spawn_window)
            // `refresh_silver_readout` belongs to `ui/inventory.rs` and is registered by
            // that plugin too. One function and two registrations rather than a second
            // purse: it is a query and a string comparison, and the alternative is a number
            // in this window that can disagree with the number in the pack.
            // Chained through `ApplyDeferred` for the reason `ui/inventory.rs`'s systems
            // are: the readout is spawned by the rebuild's deferred commands, and a refresh
            // before they are applied would leave the purse reading zero for a frame.
            .add_systems(
                Update,
                (
                    rebuild_window,
                    ApplyDeferred,
                    refresh_silver_readout,
                    show_window,
                )
                    .chain(),
            )
            .add_message::<VendorTradeClick>()
            .add_systems(Update, click_rows);
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
/// `VendorState` replaces the prices; an `InventoryState` changes which buy rows the player
/// has anything to fill, and a sell column that rebuilt only with the price list would keep
/// offering a stack that has since been spent.
fn rebuild_window(
    window: Res<VendorWindow>,
    inventory: Option<Res<Inventory>>,
    appearances: Option<Res<Appearances>>,
    roots: Query<Entity, With<VendorRoot>>,
    mut commands: Commands,
    // Optional, because the UI stands up headlessly without the player plugin that owns it.
    liveries: Option<Res<Liveries>>,
) {
    let inventory_moved = inventory.as_ref().is_some_and(DetectChanges::is_changed);
    let identity_moved = appearances.as_ref().is_some_and(DetectChanges::is_changed);
    if !window.is_changed() && !inventory_moved && !identity_moved {
        return;
    }
    for root in &roots {
        commands.entity(root).despawn_related::<Children>();
        let Some(state) = window.state() else {
            continue;
        };
        commands.entity(root).with_children(|root| {
            root.spawn((
                Text::new(vendor_title(appearances.as_deref(), state.entity_id)),
                TextFont {
                    font_size: FontSize::Px(22.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            // Every row the vendor sells, whether or not the purse covers it: whether it
            // does is the server's answer, and a list pruned here would be this client
            // refusing a trade it was never asked about.
            let buy: Vec<Row> = state.sells.iter().map(|entry| (*entry, None)).collect();
            // What the vendor buys, narrowed to what the player is carrying: a row for a
            // thing nobody holds is an offer that cannot be taken, and the count beside it
            // is the last complete state the server sent.
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
                spawn_column(columns, BUY, &buy, NOTHING_FOR_SALE, liveries.as_deref());
                spawn_column(columns, SELL, &sell, NOTHING_WANTED, liveries.as_deref());
            });
            spawn_silver_readout(root, PADDING);
        });
    }
}

/// The active resident's display identity, or the neutral title while it has not arrived.
///
/// No identity is stored beside [`VendorWindow`]. The price list and appearance streams
/// are unordered, so deriving this on either resource's change makes both arrival orders
/// the same operation and prevents a previous vendor's name surviving a switch or close.
fn vendor_title(appearances: Option<&Appearances>, entity_id: u64) -> String {
    appearances
        .and_then(|appearances| appearances.resident_label(entity_id))
        .unwrap_or_else(|| "Trade".to_owned())
}

/// How many of one item the last authoritative inventory holds, or `None` for none at all.
///
/// **Every slot, which is the total and not a second opinion about where things may sit.**
/// No row of the server's four buy lists names a wearable, so the count the server spends
/// from the pack and the count summed here are the same number for every row that can
/// appear.
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
    let buying = heading == BUY;
    columns
        .spawn(Node {
            // A basis of zero with equal growth, so the columns are halves of the window
            // rather than halves of whichever list is longer.
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
                spawn_row(column, *entry, *held, buying, liveries);
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

/// One row: the icon, the label, what the player holds of it, the silver per unit, and the
/// one button that asks for it.
///
/// ASCII throughout, and the separator is the pipe the rest of this client uses. Bevy's
/// embedded fallback font is the whole font stack here — a 95-glyph subset — so a
/// typographic dash on a price would draw as nothing at all.
fn spawn_row(
    column: &mut ChildSpawnerCommands<'_>,
    entry: VendorEntry,
    held: Option<u32>,
    buying: bool,
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
                Node {
                    flex_grow: 1.0,
                    ..default()
                },
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
            row.spawn((
                VendorRowButton {
                    item_id: entry.item_id,
                    buying,
                },
                Button,
                Node {
                    flex_shrink: 0.0,
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                    ..default()
                },
                BackgroundColor(BUTTON),
            ))
            .with_children(|button| {
                button.spawn((
                    Text::new(if buying { BUY } else { SELL }.to_owned()),
                    TextFont {
                        font_size: FontSize::Px(15.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));
            });
        });
}

/// Reports one press per button, with the amount the modifier asked for.
///
/// **Shift is read here and not in `player::vendor`**, for the reason `ui/inventory.rs`
/// reads it for a drop: which of two amounts a press meant is a fact about the press, and
/// the module that builds the frame should be told what was asked for rather than have to
/// reconstruct it. `Changed<Interaction>` is the edge every other button in this client
/// uses, so a held button is one request rather than one per frame at a stall that would
/// answer `StaleRevision` to all but the first.
fn click_rows(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut rows: Query<(&VendorRowButton, &Interaction, &mut BackgroundColor), Changed<Interaction>>,
    mut clicks: MessageWriter<VendorTradeClick>,
) {
    let many = keys
        .is_some_and(|keys| keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight));
    for (button, interaction, mut colour) in &mut rows {
        colour.0 = button_colour(interaction);
        if *interaction == Interaction::Pressed {
            clicks.write(VendorTradeClick {
                item_id: button.item_id,
                buying: button.buying,
                count: if many { SHIFT_COUNT } else { 1 },
            });
        }
    }
}

/// Shows the window only for a live session that is in the mode and has a list to draw.
///
/// **The life state is asked here as well as in `player::vendor`, deliberately.** Nothing
/// orders `reconcile_vendor` against this chain, so on the frame a death arrives this may
/// run first and draw a stall over the death overlay. Two independent expressions over one
/// fact, the rule `InputGate` states for `may_aim` and `may_act`. `SelfVitals` is optional
/// because this plugin stands up headlessly without the player plugin that owns it.
fn show_window(
    window: Res<VendorWindow>,
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    vitals: Option<Res<SelfVitals>>,
    mut roots: Query<&mut Visibility, With<VendorRoot>>,
) {
    let dead = vitals.is_some_and(|vitals| vitals.dead());
    let shown =
        session.is_some() && !dead && *mode == InputMode::Vendor && window.state().is_some();
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
    use crate::net::{ANY_TOKEN, ResidentRole, SessionParams, VendorState};
    use crate::ui::inventory::SilverCount;

    /// Three ids from the smith's list in `server/internal/game/vendor.go`. Spelled as
    /// numbers because the registry's own constants are private to `player::items`, and
    /// every assertion below reads the label back through [`item_label`] rather than
    /// hard-coding a name — so a wrong number here fails rather than passing quietly.
    const PICKAXE: u16 = 17;
    const RAW_IRON: u16 = 6;
    const RAW_COAL: u16 = 5;
    const BLACK_HORSE: u16 = 41;
    const BROWN_HORSE: u16 = 42;
    const GREY_HORSE: u16 = 43;

    const SMITH: u64 = (1 << 62) | 55;
    const COOK: u64 = (1 << 62) | 56;

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
            entity_id: SMITH,
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

    fn cook() -> VendorState {
        VendorState {
            entity_id: COOK,
            revision: 8,
            sells: vec![VendorEntry {
                item_id: PICKAXE,
                price: 30,
            }],
            buys: Vec::new(),
        }
    }

    fn stablemaster() -> VendorState {
        VendorState {
            entity_id: SMITH,
            revision: 1,
            sells: [BLACK_HORSE, BROWN_HORSE, GREY_HORSE]
                .into_iter()
                .enumerate()
                .map(|(index, item_id)| VendorEntry {
                    item_id,
                    price: 100 + index as u16,
                })
                .collect(),
            buys: Vec::new(),
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

    /// The inventory this test reads its held counts from: four raw iron, a twelve-silver
    /// purse, and no coal at all.
    fn carrying() -> Inventory {
        Inventory::from_state(vec![stack(RAW_IRON, 4)], 12)
    }

    fn app_with_appearances(appearances: Option<Appearances>) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(InputMode::Vendor)
            .insert_resource(VendorWindow::from_server(smith()))
            .insert_resource(carrying())
            .add_plugins(VendorUiPlugin);
        if let Some(appearances) = appearances {
            app.insert_resource(appearances);
        }
        app.update();
        app
    }

    fn app() -> App {
        app_with_appearances(Some(Appearances::with_resident(
            SMITH,
            "Sigrun",
            ResidentRole::Smith,
        )))
    }

    fn lines(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        let mut texts = world.query::<&Text>();
        texts.iter(world).map(|text| text.0.clone()).collect()
    }

    /// The window draws the vendor's prices, the player's holdings and their purse, and
    /// nothing at all about a row they have nothing to fill.
    #[test]
    fn both_columns_are_drawn_from_the_list_and_the_pack() {
        let mut app = app();
        let world = app.world_mut();
        let mut roots = world.query_filtered::<&Visibility, With<VendorRoot>>();
        assert_eq!(roots.single(world).unwrap(), &Visibility::Visible);

        let drawn = lines(&mut app);
        assert!(drawn.contains(&"Sigrun | Smith".to_owned()), "{drawn:?}");
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

    #[test]
    fn stablemaster_rows_use_the_canonical_breed_names() {
        let mut app = app();
        app.insert_resource(VendorWindow::from_server(stablemaster()));
        app.insert_resource(Appearances::with_resident(
            SMITH,
            "Eira",
            ResidentRole::Stablemaster,
        ));
        app.update();

        let drawn = lines(&mut app);
        assert!(
            drawn.contains(&"Eira | Stablemaster".to_owned()),
            "{drawn:?}"
        );
        for (item_id, name, price) in [
            (BLACK_HORSE, "Raven Friesian", 100),
            (BROWN_HORSE, "Chestnut Icelandic", 101),
            (GREY_HORSE, "Silver Fjord", 102),
        ] {
            assert_eq!(item_label(item_id), name);
            assert!(
                drawn.contains(&format!("{name} | {price} silver")),
                "stablemaster row {item_id} was not canonical: {drawn:?}"
            );
        }
        assert!(
            !drawn.iter().any(|line| line.contains("unknown item")),
            "a stablemaster row still fell through the registry: {drawn:?}"
        );
    }

    /// The appearance and price-list streams are unordered. Whichever arrives second
    /// refreshes the title, and a correction for the active resident does the same.
    #[test]
    fn both_arrival_orders_and_a_later_identity_refresh_draw_the_resident() {
        // Appearance first: it is already cached when the startup rebuild sees the list.
        let mut appearance_first = app();
        assert!(
            lines(&mut appearance_first).contains(&"Sigrun | Smith".to_owned()),
            "an appearance cached before the list was not used"
        );

        // List first: absence has a neutral title, then the cache change rebuilds it.
        let mut list_first = app_with_appearances(None);
        assert!(lines(&mut list_first).contains(&"Trade".to_owned()));
        assert!(
            !lines(&mut list_first)
                .iter()
                .any(|line| line.contains("revision")),
            "protocol metadata reached the fallback title"
        );
        list_first.insert_resource(Appearances::with_resident(
            SMITH,
            "Sigrun",
            ResidentRole::Smith,
        ));
        list_first.update();
        assert!(
            lines(&mut list_first).contains(&"Sigrun | Smith".to_owned()),
            "a resident arriving after the list did not refresh the title"
        );

        list_first.insert_resource(Appearances::with_resident(
            SMITH,
            "Ingrid",
            ResidentRole::Trader,
        ));
        list_first.update();
        let drawn = lines(&mut list_first);
        assert!(drawn.contains(&"Ingrid | Trader".to_owned()), "{drawn:?}");
        assert!(!drawn.contains(&"Sigrun | Smith".to_owned()), "{drawn:?}");
    }

    /// Every role that owns a stall uses the same role word as the resident's plate.
    /// An exhaustive match inside `Appearances::resident_label` keeps future roles from
    /// silently falling through to `Trade`; these are the four roles a stall has today.
    #[test]
    fn every_vendor_role_has_an_identity_title() {
        for (role, role_name) in [
            (ResidentRole::Smith, "Smith"),
            (ResidentRole::Cook, "Cook"),
            (ResidentRole::Trader, "Trader"),
            (ResidentRole::Stablemaster, "Stablemaster"),
        ] {
            let appearances = Appearances::with_resident(SMITH, "Sigrun", role);
            assert_eq!(
                vendor_title(Some(&appearances), SMITH),
                format!("Sigrun | {role_name}")
            );
        }
    }

    /// Switching directly to an undescribed vendor falls back instead of retaining the
    /// previous identity; closing the stall removes the title with the rest of its rows.
    #[test]
    fn switching_and_closing_cannot_leave_a_stale_vendor_identity() {
        let mut app = app();
        assert!(lines(&mut app).contains(&"Sigrun | Smith".to_owned()));

        app.insert_resource(VendorWindow::from_server(cook()));
        app.update();
        let drawn = lines(&mut app);
        assert!(drawn.contains(&"Trade".to_owned()), "{drawn:?}");
        assert!(!drawn.contains(&"Sigrun | Smith".to_owned()), "{drawn:?}");

        app.insert_resource(Appearances::with_resident(
            COOK,
            "Astrid",
            ResidentRole::Cook,
        ));
        app.update();
        assert!(lines(&mut app).contains(&"Astrid | Cook".to_owned()));

        app.insert_resource(VendorWindow::default());
        app.update();
        let drawn = lines(&mut app);
        assert!(!drawn.iter().any(|line| line == "Trade"), "{drawn:?}");
        assert!(
            !drawn.iter().any(|line| line.contains("Astrid")),
            "{drawn:?}"
        );
    }

    /// **Spending the last of a stack takes its row out of the sell column** — the half of
    /// "rows rebuild" a `VendorState` alone would never exercise: the pack moved, not the
    /// price list.
    #[test]
    fn a_pack_that_moved_rebuilds_the_sell_column() {
        let mut app = app();
        app.insert_resource(Inventory::from_state(Vec::new(), 12));
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

    /// **A death hides the window in the same frame it arrives**, whichever order the two
    /// plugins' systems run in. The window is left open and the mode left at `Vendor` — the
    /// state a frame starts in when a player dies with a stall up — and only the life state
    /// moves, so this fails if the visibility came from anything else.
    #[test]
    fn a_death_hides_the_window_without_waiting_for_the_player_plugin() {
        let mut app = app();
        app.insert_resource(SelfVitals::from_server(crate::net::PlayerVitals {
            health: 0,
            life_state: crate::net::LifeState::Dead,
            ..crate::net::PlayerVitals::unharmed()
        }));
        app.update();

        let world = app.world_mut();
        let mut roots = world.query_filtered::<&Visibility, With<VendorRoot>>();
        assert_eq!(roots.single(world).unwrap(), &Visibility::Hidden);
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Vendor);
        assert!(
            app.world().resource::<VendorWindow>().state().is_some(),
            "this test asserts what the UI draws, not what the player plugin cleared"
        );
    }

    /// **A press reports the row and the direction, and shift is the only thing that
    /// changes the amount.**
    ///
    /// The two buttons are found by their own component rather than by position, and the
    /// direction is asserted on both, because an item can be in a vendor's two vectors at
    /// different prices and a press that got the direction from the column it was drawn in
    /// would be reading a layout rather than a row.
    #[test]
    fn a_press_reports_the_row_the_direction_and_the_modifier() {
        let mut app = app();
        app.insert_resource(ButtonInput::<KeyCode>::default());
        app.update();

        for (item_id, buying, count, held) in [
            (PICKAXE, true, 1u16, None),
            (RAW_IRON, false, SHIFT_COUNT, Some(KeyCode::ShiftLeft)),
            (PICKAXE, true, SHIFT_COUNT, Some(KeyCode::ShiftRight)),
        ] {
            {
                let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
                keys.clear();
                keys.release_all();
                if let Some(key) = held {
                    keys.press(key);
                }
            }
            press(&mut app, item_id, buying);
            assert_eq!(
                app.world_mut()
                    .resource_mut::<Messages<VendorTradeClick>>()
                    .drain()
                    .collect::<Vec<_>>(),
                [VendorTradeClick {
                    item_id,
                    buying,
                    count
                }]
            );
        }
    }

    /// Presses the one button drawn for that row and runs the frame that reads it.
    fn press(app: &mut App, item_id: u16, buying: bool) {
        let world = app.world_mut();
        let mut buttons = world.query::<(&VendorRowButton, &mut Interaction)>();
        let mut found = false;
        for (button, mut interaction) in buttons.iter_mut(world) {
            let mine = button.item_id == item_id && button.buying == buying;
            // Every button is written every pass, so `Changed<Interaction>` fires for the
            // one under test rather than for whichever happened to differ from last time.
            *interaction = if mine {
                found = true;
                Interaction::Pressed
            } else {
                Interaction::None
            };
        }
        assert!(
            found,
            "no button was drawn for item {item_id} buying {buying}"
        );
        app.update();
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
