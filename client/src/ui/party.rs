//! The permanent, display-only mirror of the server's party snapshot.

use bevy::prelude::*;

use crate::net::Session;
use crate::player::{Appearances, InputMode, Party};

const ROW_COUNT: usize = 4;
const TOP: f32 = 48.0;
const RIGHT: f32 = 16.0;
const ROW_WIDTH: f32 = 230.0;
const BAR_WIDTH: f32 = 88.0;
const BAR_HEIGHT: f32 = 10.0;
const TRACK: Color = Color::srgba(0.055, 0.065, 0.080, 0.94);
const FILL: Color = Color::srgb(0.72, 0.16, 0.16);
const DEAD: Color = Color::srgb(0.46, 0.48, 0.52);
const ALIVE: Color = Color::WHITE;
const PLACEHOLDER_NAME: &str = "Unknown";

#[derive(Component)]
struct PartyRow(usize);

#[derive(Component)]
struct PartyLabel(usize);

#[derive(Component)]
struct PartyFill(usize);

pub(super) struct PartyUiPlugin;

impl Plugin for PartyUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Party>()
            .init_resource::<Appearances>()
            .init_resource::<InputMode>()
            .add_systems(Startup, spawn_party)
            .add_systems(Update, refresh_party);
    }
}

fn spawn_party(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(TOP),
                right: Val::Px(RIGHT),
                width: Val::Px(ROW_WIDTH),
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                ..default()
            },
            GlobalZIndex(12),
        ))
        .with_children(|root| {
            for index in 0..ROW_COUNT {
                root.spawn((
                    PartyRow(index),
                    Node {
                        width: Val::Percent(100.0),
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(3.0),
                        padding: UiRect::all(Val::Px(6.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.78)),
                    Visibility::Hidden,
                ))
                .with_children(|row| {
                    row.spawn((
                        PartyLabel(index),
                        Text::new(String::new()),
                        TextFont {
                            font_size: FontSize::Px(16.0),
                            ..default()
                        },
                        TextColor(ALIVE),
                        TextShadow::default(),
                    ));
                    row.spawn((
                        Node {
                            width: Val::Px(BAR_WIDTH),
                            height: Val::Px(BAR_HEIGHT),
                            ..default()
                        },
                        BackgroundColor(TRACK),
                    ))
                    .with_child((
                        PartyFill(index),
                        Node {
                            width: Val::Percent(0.0),
                            height: Val::Percent(100.0),
                            ..default()
                        },
                        BackgroundColor(FILL),
                    ));
                });
            }
        });
}

fn refresh_party(
    party: Res<Party>,
    appearances: Res<Appearances>,
    session: Option<Res<Session>>,
    mode: Res<InputMode>,
    mut rows: Query<(&PartyRow, &mut Visibility)>,
    mut labels: Query<(&PartyLabel, &mut Text, &mut TextColor)>,
    mut fills: Query<(&PartyFill, &mut Node, &mut BackgroundColor)>,
) {
    let shown = session.is_some() && matches!(*mode, InputMode::Playing | InputMode::Chat);
    for (row, mut visibility) in &mut rows {
        *visibility = if shown && row.0 < party.members.len() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    for (slot, mut text, mut colour) in &mut labels {
        let Some(member) = party.members.get(slot.0) else {
            text.0.clear();
            continue;
        };
        let (name, level) = appearances
            .identity(member.entity_id)
            .unwrap_or_else(|| (PLACEHOLDER_NAME.to_owned(), 0));
        let crown = if party.leader_entity_id == member.entity_id {
            "♛ "
        } else {
            ""
        };
        text.0 = format!("{crown}{name} · Lv {level}");
        colour.0 = if member.alive { ALIVE } else { DEAD };
    }
    for (slot, mut node, mut colour) in &mut fills {
        node.width = Val::Percent(party.members.get(slot.0).map_or(0.0, |member| {
            member.health as f32 * 100.0 / member.max_health as f32
        }));
        colour.0 = party
            .members
            .get(slot.0)
            .map_or(FILL, |member| if member.alive { FILL } else { DEAD });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, PartyMemberState, SessionParams};

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.5, 64.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 36,
            hotbar_slots: 9,
            equipment_slots: 3,
            player_token: ANY_TOKEN,
        })
    }

    fn member(entity_id: u64, health: u16, max_health: u16, alive: bool) -> PartyMemberState {
        PartyMemberState {
            entity_id,
            pos: [0.0, 64.0, 0.0],
            health,
            max_health,
            alive,
        }
    }

    #[test]
    fn four_permanent_rows_show_server_health_leader_and_life_state() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(InputMode::Playing)
            .insert_resource(Party {
                leader_entity_id: 9,
                members: vec![member(9, 25, 100, true), member(11, 0, 80, false)],
            })
            .add_plugins(PartyUiPlugin);
        app.update();

        let world = app.world_mut();
        let mut rows = world.query::<(&PartyRow, &Visibility)>();
        let visible = rows
            .iter(world)
            .filter(|(_, visibility)| **visibility == Visibility::Visible)
            .count();
        assert_eq!(rows.iter(world).count(), ROW_COUNT);
        assert_eq!(visible, 2);

        let mut labels = world.query::<(&PartyLabel, &Text, &TextColor)>();
        let (_, leader, leader_colour) =
            labels.iter(world).find(|(slot, _, _)| slot.0 == 0).unwrap();
        assert_eq!(leader.0, "♛ Unknown · Lv 0");
        assert_eq!(leader_colour.0, ALIVE);
        let (_, dead, dead_colour) = labels.iter(world).find(|(slot, _, _)| slot.0 == 1).unwrap();
        assert_eq!(dead.0, "Unknown · Lv 0");
        assert_eq!(dead_colour.0, DEAD);

        let mut fills = world.query::<(&PartyFill, &Node, &BackgroundColor)>();
        let (_, fill, _) = fills.iter(world).find(|(slot, _, _)| slot.0 == 0).unwrap();
        assert_eq!(fill.width, Val::Percent(25.0));
    }
}
