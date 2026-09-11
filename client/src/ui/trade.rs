use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;
use bevy::ui::FocusPolicy;

use super::text_input::{
    FieldInput, FieldPieces, FieldSpan, TextEdit, TextField, paint_span, spawn_field_spans,
};
use super::{BUTTON, CELL_EDGE, button_colour, cell_node, icon, stack_style};
use crate::net::{InventoryStack, PLAYER_TRADE_SLOTS, PlayerTradeSlot, Session};
use crate::player::{
    InputMode, Inventory, Liveries, PlayerTradeClick, PlayerTradeWindow, SelfVitals,
};

const WIDTH: f32 = 720.0;
const PADDING: f32 = 16.0;
const GAP: f32 = 8.0;
const LOCKED: Color = Color::srgba(0.10, 0.11, 0.13, 0.96);
const CONFIRMED: Color = Color::srgba(0.42, 0.31, 0.15, 0.32);

#[derive(Component)]
struct TradeRoot;

#[derive(Component)]
struct TradePress {
    click: PlayerTradeClick,
    locked: bool,
}

#[derive(Component)]
struct SilverField;

/// One of the spans the silver amount is drawn in.
#[derive(Component, Clone)]
struct SilverFieldText;

/// The silver amount's spans, which [`edit_silver`] repaints.
type SilverSpans<'w, 's> = Query<
    'w,
    's,
    (
        &'static FieldSpan,
        &'static mut TextSpan,
        &'static mut TextColor,
        &'static mut TextBackgroundColor,
    ),
    With<SilverFieldText>,
>;

#[derive(Resource, Debug, Default)]
struct SilverDraft {
    seed: Option<(u64, u32)>,
    line: TextField,
    focused: bool,
}

pub(super) struct PlayerTradeUiPlugin;

impl Plugin for PlayerTradeUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerTradeWindow>()
            .init_resource::<Inventory>()
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<SilverDraft>()
            .add_message::<PlayerTradeClick>()
            .add_message::<KeyboardInput>()
            .add_systems(Startup, spawn_window)
            .add_systems(
                Update,
                (
                    rebuild_window,
                    ApplyDeferred,
                    edit_silver,
                    click_controls,
                    show_window,
                )
                    .chain(),
            );
    }
}

fn spawn_window(mut commands: Commands) {
    commands.spawn((
        TradeRoot,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(WIDTH),
            max_height: Val::Percent(80.0),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(12.0),
            padding: UiRect::all(Val::Px(PADDING)),
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
    window: Res<PlayerTradeWindow>,
    inventory: Res<Inventory>,
    session: Option<Res<Session>>,
    mut draft: ResMut<SilverDraft>,
    roots: Query<Entity, With<TradeRoot>>,
    mut commands: Commands,
    liveries: Option<Res<Liveries>>,
) {
    let session_moved = session.as_ref().is_some_and(DetectChanges::is_changed);
    if !window.is_changed() && !inventory.is_changed() && !session_moved {
        return;
    }
    for root in &roots {
        commands.entity(root).despawn_related::<Children>();
        let (Some(state), Some(session)) = (window.state(), session.as_deref()) else {
            *draft = SilverDraft::default();
            continue;
        };
        let seed = (state.partner_entity_id, state.my_silver);
        if draft.seed != Some(seed) {
            draft.seed = Some(seed);
            draft.line.set_text(&state.my_silver.to_string());
            draft.focused = false;
        }

        commands.entity(root).with_children(|root| {
            root.spawn(trade_text(
                format!("Trade with {}", state.partner_name),
                22.0,
            ));
            root.spawn(Node {
                width: Val::Percent(100.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(16.0),
                ..default()
            })
            .with_children(|columns| {
                spawn_offer_column(
                    columns,
                    "Mine",
                    &state.my_offer,
                    state.my_silver,
                    state.my_confirmed,
                    true,
                    &draft.line.pieces(draft.focused),
                    liveries.as_deref(),
                );
                spawn_offer_column(
                    columns,
                    &state.partner_name,
                    &state.their_offer,
                    state.their_silver,
                    state.their_confirmed,
                    false,
                    &TextField::default().pieces(false),
                    liveries.as_deref(),
                );
            });
            root.spawn(Node {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                ..default()
            })
            .with_children(|buttons| {
                spawn_button(
                    buttons,
                    "Trade",
                    PlayerTradeClick::Confirm,
                    state.my_confirmed,
                );
                spawn_button(buttons, "Cancel", PlayerTradeClick::Cancel, false);
            });

            root.spawn(trade_text("Pack", 18.0));
            let equipment_first = session.0.inventory_slots - session.0.equipment_slots;
            root.spawn((
                Node {
                    width: Val::Percent(100.0),
                    display: Display::Grid,
                    grid_template_columns: RepeatedGridTrack::flex(
                        u16::from(session.0.hotbar_slots),
                        1.0,
                    ),
                    row_gap: Val::Px(6.0),
                    column_gap: Val::Px(6.0),
                    padding: UiRect::all(Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(if state.my_confirmed {
                    CONFIRMED
                } else {
                    Color::NONE
                }),
            ))
            .with_children(|grid| {
                for slot in session.0.hotbar_slots..equipment_first {
                    let stack = inventory.slot(slot);
                    let offered = state.my_offer.iter().any(|offer| offer.pack_slot == slot);
                    let empty = stack.is_none_or(|stack| stack.item_id == 0 || stack.count == 0);
                    spawn_stack_cell(
                        grid,
                        stack,
                        TradePress {
                            click: PlayerTradeClick::OfferPackSlot(slot),
                            locked: state.my_confirmed || offered || empty,
                        },
                        offered,
                        liveries.as_deref(),
                    );
                }
            });
        });
    }
}

#[allow(clippy::too_many_arguments, reason = "offer")]
fn spawn_offer_column(
    columns: &mut ChildSpawnerCommands<'_>,
    heading: &str,
    offer: &[PlayerTradeSlot],
    silver: u32,
    confirmed: bool,
    mine: bool,
    silver_line: &FieldPieces,
    liveries: Option<&Liveries>,
) {
    columns
        .spawn((
            Node {
                flex_basis: Val::Px(0.0),
                flex_grow: 1.0,
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(GAP),
                padding: UiRect::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(if confirmed { CONFIRMED } else { Color::NONE }),
        ))
        .with_children(|column| {
            column.spawn(trade_text(heading, 17.0));
            column
                .spawn(Node {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(5.0),
                    ..default()
                })
                .with_children(|slots| {
                    for trade_slot in 0..PLAYER_TRADE_SLOTS as u8 {
                        let entry = offer.iter().find(|entry| entry.trade_slot == trade_slot);
                        let stack = entry.map(trade_stack);
                        let press = TradePress {
                            click: PlayerTradeClick::ClearOfferSlot(trade_slot),
                            locked: !mine || confirmed || entry.is_none(),
                        };
                        spawn_stack_cell(slots, stack, press, false, liveries);
                    }
                });
            if mine {
                column
                    .spawn((
                        SilverField,
                        Button,
                        Node {
                            width: Val::Percent(100.0),
                            padding: UiRect::axes(Val::Px(8.0), Val::Px(6.0)),
                            ..default()
                        },
                        BackgroundColor(if confirmed { LOCKED } else { BUTTON }),
                    ))
                    .with_children(|field| {
                        let (label, font, colour) = trade_text("Silver: ", 15.0);
                        field
                            .spawn((label, font.clone(), colour, FocusPolicy::Pass))
                            .with_children(|label| {
                                spawn_field_spans(
                                    label,
                                    silver_line,
                                    &font,
                                    colour.0,
                                    SilverFieldText,
                                );
                            });
                    });
            } else {
                column.spawn(trade_text(format!("Silver: {silver}"), 15.0));
            }
        });
}

fn trade_text(text: impl Into<String>, size: f32) -> (Text, TextFont, TextColor) {
    (
        Text::new(text),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(Color::WHITE),
    )
}

fn trade_stack(slot: &PlayerTradeSlot) -> InventoryStack {
    InventoryStack {
        item_id: slot.item_id,
        count: slot.count,
        durability: slot.durability,
        max_durability: slot.max_durability,
    }
}

fn spawn_stack_cell(
    parent: &mut ChildSpawnerCommands<'_>,
    stack: Option<InventoryStack>,
    press: TradePress,
    offered: bool,
    liveries: Option<&Liveries>,
) {
    let style = stack_style(stack);
    let locked = press.locked;
    let details = stack.map_or(style.count.clone(), |stack| {
        if stack.max_durability > 0 {
            format!(
                "{}  {}/{}",
                style.count, stack.durability, stack.max_durability
            )
        } else {
            style.count.clone()
        }
    });
    parent
        .spawn((
            press,
            Button,
            cell_node(),
            BackgroundColor(if offered || locked {
                LOCKED
            } else {
                style.background
            }),
            BorderColor::all(CELL_EDGE),
        ))
        .with_children(|cell| {
            if let Some(icon) = style.icon {
                cell.spawn(icon::host_bundle())
                    .with_children(|host| icon::spawn(host, icon, liveries));
            }
            cell.spawn((
                trade_text(details, 12.0),
                Node {
                    position_type: PositionType::Absolute,
                    right: Val::Px(2.0),
                    bottom: Val::Px(1.0),
                    ..default()
                },
                FocusPolicy::Pass,
            ));
        });
}

fn spawn_button(
    parent: &mut ChildSpawnerCommands<'_>,
    label: &str,
    click: PlayerTradeClick,
    locked: bool,
) {
    parent
        .spawn((
            TradePress { click, locked },
            Button,
            Node {
                padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(if locked { LOCKED } else { BUTTON }),
        ))
        .with_child(trade_text(label, 16.0));
}

fn edit_silver(
    window: Res<PlayerTradeWindow>,
    mut draft: ResMut<SilverDraft>,
    mut keys: MessageReader<KeyboardInput>,
    mut field: FieldInput,
    mut fields: Query<(&Interaction, &mut BackgroundColor), With<SilverField>>,
    mut labels: SilverSpans,
    mut clicks: MessageWriter<PlayerTradeClick>,
) {
    let events: Vec<KeyboardInput> = keys.read().cloned().collect();
    let Some(state) = window.state() else {
        draft.focused = false;
        return;
    };
    let Ok((interaction, mut colour)) = fields.single_mut() else {
        return;
    };
    if state.my_confirmed {
        draft.focused = false;
        colour.0 = LOCKED;
        draw_silver(&draft.line, false, &mut labels);
        return;
    }
    colour.0 = if draft.focused {
        super::BUTTON_HOVERED
    } else {
        button_colour(interaction)
    };
    if *interaction == Interaction::Pressed {
        draft.focused = true;
    }

    let mut submit = false;
    if draft.focused {
        for event in &events {
            match field.apply(&mut draft.line, event, 10) {
                Some(TextEdit::Typed) => keep_digits(&mut draft.line),
                Some(TextEdit::Submitted) => submit = true,
                Some(TextEdit::Cancelled) | None => {}
            }
        }
    }
    if submit {
        draft.focused = false;
        if let Ok(silver) = draft.line.text().parse::<u32>() {
            clicks.write(PlayerTradeClick::SetSilver(silver));
        }
    }
    draw_silver(&draft.line, draft.focused, &mut labels);
}

/// Draws the amount, with a caret and a selection only while the field has the keyboard.
fn draw_silver(line: &TextField, focused: bool, labels: &mut SilverSpans) {
    let pieces = line.pieces(focused);
    for (slot, span, colour, background) in labels {
        if let Some(piece) = pieces.get(slot.0) {
            paint_span(piece, Color::WHITE, span, colour, background);
        }
    }
}

/// Drops everything but digits, which is the whole of what a silver amount may hold.
///
/// Only when something has to go, because replacing the line puts the cursor at its end: an
/// ordinary digit typed in the middle of the amount keeps the cursor where the player put it.
fn keep_digits(line: &mut TextField) {
    if !line.text().bytes().all(|byte| byte.is_ascii_digit()) {
        let digits: String = line.text().chars().filter(char::is_ascii_digit).collect();
        line.set_text(&digits);
    }
}

fn click_controls(
    mut buttons: Query<(&TradePress, &Interaction, &mut BackgroundColor), Changed<Interaction>>,
    mut clicks: MessageWriter<PlayerTradeClick>,
) {
    for (press, interaction, mut colour) in &mut buttons {
        if !press.locked {
            colour.0 = button_colour(interaction);
        }
        if *interaction == Interaction::Pressed && !press.locked {
            clicks.write(press.click);
        }
    }
}

fn show_window(
    window: Res<PlayerTradeWindow>,
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut roots: Query<&mut Visibility, With<TradeRoot>>,
) {
    let shown = session.is_some()
        && !vitals.dead()
        && *mode == InputMode::Trade
        && window.state().is_some();
    for mut visibility in &mut roots {
        *visibility = if shown {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<SilverDraft>(world);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, PlayerTradeState, SessionParams};

    fn session() -> Session {
        Session(SessionParams {
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
            clock: Default::default(),
            voice_range_blocks: 0.0,
        })
    }

    fn state() -> PlayerTradeState {
        let slot = PlayerTradeSlot {
            trade_slot: 2,
            pack_slot: 9,
            item_id: 3,
            count: 2,
            durability: 5,
            max_durability: 10,
        };
        PlayerTradeState {
            partner_entity_id: 11,
            partner_name: "Eirik".to_owned(),
            revision: 4,
            my_offer: vec![slot],
            their_offer: vec![PlayerTradeSlot {
                pack_slot: 0,
                ..slot
            }],
            my_silver: 12,
            their_silver: 23,
            my_confirmed: false,
            their_confirmed: true,
        }
    }

    #[test]
    fn the_window_draws_five_slots_per_side_the_pack_and_authoritative_locks() {
        let mut stacks = vec![InventoryStack::default(); 37];
        stacks[9] = InventoryStack {
            item_id: 3,
            count: 2,
            durability: 5,
            max_durability: 10,
        };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(InputMode::Trade)
            .insert_resource(Inventory::from_state(stacks, 99))
            .insert_resource(PlayerTradeWindow::from_server(state()))
            .add_plugins(PlayerTradeUiPlugin);
        app.update();

        let world = app.world_mut();
        let mut presses = world.query::<(&TradePress, &BackgroundColor)>();
        let drawn: Vec<_> = presses.iter(world).collect();
        let count = |kind: fn(PlayerTradeClick) -> bool| {
            drawn.iter().filter(|(press, _)| kind(press.click)).count()
        };
        assert_eq!(
            count(|click| matches!(click, PlayerTradeClick::ClearOfferSlot(_))),
            10
        );
        assert_eq!(
            count(|click| matches!(click, PlayerTradeClick::OfferPackSlot(_))),
            24
        );
        assert!(drawn.iter().any(|(press, colour)| {
            press.click == PlayerTradeClick::OfferPackSlot(9) && colour.0 == LOCKED
        }));
        let mut texts = world.query::<&Text>();
        let text: Vec<_> = texts.iter(world).map(|text| text.0.as_str()).collect();
        assert!(text.contains(&"Trade with Eirik"));
        assert!(text.contains(&"Silver: ") && text.contains(&"Silver: 23"));
        assert!(text.iter().any(|line| line.contains("5/10")));

        let mut mine = world.query_filtered::<(&FieldSpan, &TextSpan), With<SilverFieldText>>();
        let mut spans: Vec<_> = mine
            .iter(world)
            .map(|(slot, span)| (slot.0, span.0.clone()))
            .collect();
        spans.sort_by_key(|span| span.0);
        assert_eq!(
            spans.into_iter().map(|span| span.1).collect::<String>(),
            "12",
            "the amount is drawn in its spans, and unfocused it has no caret"
        );
    }
}
