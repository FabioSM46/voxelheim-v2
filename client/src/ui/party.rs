//! The permanent, display-only mirror of the server's party snapshot.

use bevy::prelude::*;

use crate::net::{LifeState, Session};
use crate::player::{Appearances, ApplySnapshots, InputMode, Party, SelfVitals, SnapshotBuffer};

const ROW_COUNT: usize = 4;
const TOP: f32 = 48.0;
const RIGHT: f32 = 16.0;
const ROW_WIDTH: f32 = 230.0;
const BAR_WIDTH: f32 = 88.0;
const BAR_HEIGHT: f32 = 10.0;
const TRACK: Color = Color::srgba(0.055, 0.065, 0.080, 0.94);
const FILL: Color = Color::srgb(0.72, 0.16, 0.16);
const DEAD: Color = Color::srgb(0.46, 0.48, 0.52);
const OFFLINE: Color = Color::srgb(0.42, 0.44, 0.48);
const ALIVE: Color = Color::WHITE;
const PLACEHOLDER_NAME: &str = "Unknown";
const HUNTED_MARK: &str = "⚔ ";

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
            .init_resource::<SelfVitals>()
            .init_resource::<InputMode>()
            .init_resource::<SnapshotBuffer>()
            .add_systems(Startup, spawn_party)
            // The mark reads mob targets from the newest snapshot accepted this frame.
            // Without the order, a switch from A to B may spend one drawn frame on A.
            .add_systems(Update, refresh_party.after(ApplySnapshots));
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
                        Node {
                            width: Val::Percent(100.0),
                            overflow: Overflow::clip(),
                            ..default()
                        },
                        Text::new(String::new()),
                        TextFont {
                            font_size: FontSize::Px(16.0),
                            ..default()
                        },
                        TextColor(ALIVE),
                        TextLayout::no_wrap(),
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

#[derive(bevy::ecs::system::SystemParam)]
struct PartyView<'w> {
    party: Res<'w, Party>,
    appearances: Res<'w, Appearances>,
    vitals: Res<'w, SelfVitals>,
    session: Option<Res<'w, Session>>,
    mode: Res<'w, InputMode>,
    snapshots: Res<'w, SnapshotBuffer>,
}

fn refresh_party(
    view: PartyView<'_>,
    mut rows: Query<(&PartyRow, &mut Visibility)>,
    mut labels: Query<(&PartyLabel, &mut Text, &mut TextColor)>,
    mut fills: Query<(&PartyFill, &mut Node, &mut BackgroundColor)>,
) {
    let PartyView {
        party,
        appearances,
        vitals,
        session,
        mode,
        snapshots,
    } = view;
    let shown = session.is_some() && matches!(*mode, InputMode::Playing | InputMode::Chat);
    for (row, mut visibility) in &mut rows {
        *visibility = if shown && row.0 < party.roster.len() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    for (slot, mut text, mut colour) in &mut labels {
        let Some(member) = party.roster.get(slot.0) else {
            text.0.clear();
            continue;
        };
        let hunted = if snapshots.mob_hunts(member.entity_id) {
            HUNTED_MARK
        } else {
            ""
        };
        let crown = if slot.0 == 0 { "♛ " } else { "" };
        if !member.online {
            text.0 = format!("{hunted}{crown}{} · offline", member.name);
            colour.0 = OFFLINE;
            continue;
        }
        let level = if session
            .as_deref()
            .is_some_and(|session| session.0.entity_id == member.entity_id)
        {
            vitals.get().map_or(0, |vitals| vitals.level)
        } else {
            appearances
                .identity(member.entity_id)
                .map_or(0, |(_, level)| level)
        };
        let name = if member.name.is_empty() {
            PLACEHOLDER_NAME
        } else {
            &member.name
        };
        text.0 = format!("{hunted}{crown}{name} · Lv {level}");
        let alive = if session
            .as_deref()
            .is_some_and(|session| session.0.entity_id == member.entity_id)
        {
            vitals
                .get()
                .is_some_and(|vitals| vitals.life_state != LifeState::Dead)
        } else {
            party
                .members
                .iter()
                .find(|live| live.entity_id == member.entity_id)
                .is_some_and(|live| live.alive)
        };
        colour.0 = if alive { ALIVE } else { DEAD };
    }
    for (slot, mut node, mut colour) in &mut fills {
        let values = party.roster.get(slot.0).and_then(|member| {
            if !member.online {
                return None;
            }
            if session
                .as_deref()
                .is_some_and(|session| session.0.entity_id == member.entity_id)
            {
                vitals.get().map(|vitals| {
                    (
                        vitals.health,
                        vitals.max_health,
                        vitals.life_state != LifeState::Dead,
                    )
                })
            } else {
                party
                    .members
                    .iter()
                    .find(|live| live.entity_id == member.entity_id)
                    .map(|live| (live.health, live.max_health, live.alive))
            }
        });
        node.width = Val::Percent(values.map_or(0.0, |(health, maximum, _)| {
            health as f32 * 100.0 / maximum as f32
        }));
        colour.0 = values.map_or(OFFLINE, |(_, _, alive)| if alive { FILL } else { DEAD });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::net::{
        ANY_TOKEN, MobAction, MobKind, MobState, PartyMemberState, PartyRosterMember,
        SessionParams, Snapshot,
    };

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.5, 64.0, 0.5],
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

    fn member(entity_id: u64, health: u16, max_health: u16, alive: bool) -> PartyMemberState {
        PartyMemberState {
            entity_id,
            pos: [0.0, 64.0, 0.0],
            health,
            max_health,
            alive,
        }
    }

    fn hunter(target_entity_id: u64) -> MobState {
        MobState {
            entity_id: 900,
            kind: MobKind::Draugr,
            pos: [2.0, 64.0, 0.0],
            vel: [0.0; 3],
            yaw: 0.0,
            health: 60,
            max_health: 60,
            action: MobAction::Chase,
            target_entity_id,
        }
    }

    fn accept(app: &mut App, tick: u32, target_entity_id: u64) {
        app.world_mut().resource_mut::<SnapshotBuffer>().accept(
            Snapshot {
                server_tick: tick,
                mobs: vec![hunter(target_entity_id)],
                ..Default::default()
            },
            Instant::now(),
        );
    }

    fn labels(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        let mut labels = world.query::<(&PartyLabel, &Text)>();
        let mut labels: Vec<_> = labels
            .iter(world)
            .map(|(slot, text)| (slot.0, text.0.clone()))
            .collect();
        labels.sort_by_key(|(slot, _)| *slot);
        labels.into_iter().map(|(_, text)| text).collect()
    }

    #[test]
    fn four_permanent_rows_show_an_offline_leader_and_live_server_health() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(InputMode::Playing)
            .insert_resource(Party {
                roster: vec![
                    PartyRosterMember {
                        character_id: 90,
                        entity_id: 0,
                        name: "Skald".to_owned(),
                        online: false,
                    },
                    PartyRosterMember {
                        character_id: 110,
                        entity_id: 11,
                        name: "Hunter".to_owned(),
                        online: true,
                    },
                ],
                members: vec![member(11, 0, 80, false)],
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
        assert_eq!(leader.0, "♛ Skald · offline");
        assert_eq!(leader_colour.0, OFFLINE);
        let (_, dead, dead_colour) = labels.iter(world).find(|(slot, _, _)| slot.0 == 1).unwrap();
        assert_eq!(dead.0, "Hunter · Lv 0");
        assert_eq!(dead_colour.0, DEAD);

        let mut label_bounds = world.query::<(&PartyLabel, &Node, &TextLayout)>();
        for (_, node, layout) in label_bounds.iter(world) {
            assert_eq!(node.width, Val::Percent(100.0));
            assert_eq!(node.overflow, Overflow::clip());
            assert_eq!(layout.linebreak, LineBreak::NoWrap);
        }

        let mut fills = world.query::<(&PartyFill, &Node, &BackgroundColor)>();
        let (_, fill, _) = fills.iter(world).find(|(slot, _, _)| slot.0 == 0).unwrap();
        assert_eq!(fill.width, Val::Percent(0.0));
    }

    #[test]
    fn the_newest_mob_targets_move_the_mark_between_party_rows_in_one_frame() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(InputMode::Playing)
            .insert_resource(Party {
                roster: vec![
                    PartyRosterMember {
                        character_id: 70,
                        entity_id: 7,
                        name: "Local".to_owned(),
                        online: true,
                    },
                    PartyRosterMember {
                        character_id: 110,
                        entity_id: 11,
                        name: "Tank".to_owned(),
                        online: true,
                    },
                ],
                members: vec![member(11, 80, 80, true)],
            })
            .add_plugins(PartyUiPlugin);

        accept(&mut app, 1, 7);
        app.update();
        let first = labels(&mut app);
        assert!(first[0].starts_with(HUNTED_MARK), "local row: {}", first[0]);
        assert!(!first[1].starts_with(HUNTED_MARK), "tank row: {}", first[1]);

        accept(&mut app, 2, 11);
        app.update();
        let switched = labels(&mut app);
        assert!(!switched[0].starts_with(HUNTED_MARK));
        assert!(switched[1].starts_with(HUNTED_MARK));

        accept(&mut app, 3, 0);
        app.update();
        assert!(
            labels(&mut app)
                .iter()
                .all(|label| !label.starts_with(HUNTED_MARK)),
            "a targetless snapshot left a party mark"
        );
    }
}
