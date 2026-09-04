//! The permanent, display-only mirror of the server's party snapshot.
//!
//! ## Two of the marks on a row are drawn, not typed
//!
//! The crown on the leader and the crossed swords on whoever a mob is hunting used to be
//! `\u{265b}` and `\u{2694}` inside the row's own string. Bevy's `default_font` is the whole font
//! stack this client has, and it is a 95-glyph ASCII subset of FiraMono: neither codepoint
//! is in it, and a missing glyph lays out with **zero advance** rather than a box, so both
//! marks were absent from the screen while being present in the source. Nothing failed and
//! nothing logged - see #481.
//!
//! So they are `bevy_ui` nodes now, in the style `ui/icon.rs` established for item
//! pictures: a handful of coloured rectangles positioned in percentages of a small square,
//! some rotated, no image and no asset. They live here rather than in `icon.rs` because
//! that module answers "what does one [`crate::player::ItemShape`] look like in a cell",
//! and a party mark is neither an item nor a cell.
//!
//! **They take the row's own colour rather than one of their own.** The characters they
//! replace inherited [`TextColor`], so the alive / dead / offline signal reached them for
//! free; giving a drawn mark a palette entry would have quietly dropped that signal on the
//! two rows that carry a mark.

use bevy::math::Rot2;
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

/// The label's font size, in logical pixels.
const LABEL_SIZE: f32 = 16.0;

/// One drawn mark's box, in logical pixels. Square, and the label's size, so a mark sits on
/// the line rather than beside it.
const MARK_SIZE: f32 = LABEL_SIZE;

/// The gap between a mark and whatever follows it on the line, in logical pixels.
const MARK_GAP: f32 = 4.0;

/// The angle the two blades of the hunted mark cross at.
const QUARTER_TURN: f32 = std::f32::consts::FRAC_PI_4;

/// One rectangle of a drawn mark, as percentages of the mark's own square.
///
/// The same shape of description `ui/icon.rs` uses for an item picture, minus the parts of
/// it a party mark has no use for: there is no shading and no livery here, because a mark
/// is one flat silhouette in the row's colour.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MarkPart {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    /// Corner rounding as a percentage of the part's shorter side. `50` is a circle.
    radius: f32,
    /// Clockwise rotation about the part's own centre, in radians.
    rotation: f32,
}

impl MarkPart {
    /// A square-cornered, unrotated part. Every drawing below is written as a deviation
    /// from this, so a part states only what makes it different.
    const PLAIN: Self = Self {
        left: 0.0,
        top: 0.0,
        width: 0.0,
        height: 0.0,
        radius: 0.0,
        rotation: 0.0,
    };
}

/// Two blades crossing: what `\u{2694}` said, as geometry.
///
/// Each bar is 72% of the square tall so that a quarter turn keeps its corners inside the
/// box - a full-height bar rotated 45 degrees has a half-diagonal of 0.71 of the square and
/// would hang out of it on both ends.
const CROSSED_SWORDS: [MarkPart; 2] = [
    MarkPart {
        left: 43.0,
        top: 14.0,
        width: 14.0,
        height: 72.0,
        rotation: QUARTER_TURN,
        ..MarkPart::PLAIN
    },
    MarkPart {
        left: 43.0,
        top: 14.0,
        width: 14.0,
        height: 72.0,
        rotation: -QUARTER_TURN,
        ..MarkPart::PLAIN
    },
];

/// A band with three points on it: what `\u{265b}` said, as geometry. The middle point is the
/// tallest, which is what makes the silhouette read as a crown at fourteen pixels.
const CROWN: [MarkPart; 4] = [
    MarkPart {
        left: 6.0,
        top: 62.0,
        width: 88.0,
        height: 26.0,
        radius: 12.0,
        ..MarkPart::PLAIN
    },
    MarkPart {
        left: 6.0,
        top: 30.0,
        width: 21.0,
        height: 34.0,
        ..MarkPart::PLAIN
    },
    MarkPart {
        left: 39.0,
        top: 14.0,
        width: 22.0,
        height: 50.0,
        ..MarkPart::PLAIN
    },
    MarkPart {
        left: 73.0,
        top: 30.0,
        width: 21.0,
        height: 34.0,
        ..MarkPart::PLAIN
    },
];

/// The two marks a row can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// A mob in the newest accepted snapshot is chasing this member.
    Hunted,
    /// This member is the party's leader, which is the first row of the roster.
    Leader,
}

impl Mark {
    /// The rectangles this mark is drawn from.
    ///
    /// Exhaustive with no wildcard arm, for the reason `icon::parts` is: a third mark does
    /// not compile until somebody has drawn it.
    const fn parts(self) -> &'static [MarkPart] {
        match self {
            Self::Hunted => &CROSSED_SWORDS,
            Self::Leader => &CROWN,
        }
    }
}

#[derive(Component)]
struct PartyRow(usize);

/// The line a row's marks and name share.
#[derive(Component)]
struct PartyNameLine;

#[derive(Component)]
struct PartyLabel(usize);

#[derive(Component)]
struct PartyFill(usize);

/// One drawn mark's host node: which row it belongs to, and which mark it is.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct PartyMark {
    row: usize,
    kind: Mark,
}

/// One rectangle of a drawn mark, tagged with its row so it can follow that row's colour.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct PartyMarkPart(usize);

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
                        PartyNameLine,
                        Node {
                            width: Val::Percent(100.0),
                            display: Display::Flex,
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(MARK_GAP),
                            overflow: Overflow::clip(),
                            ..default()
                        },
                    ))
                    .with_children(|line| {
                        spawn_mark(line, index, Mark::Hunted);
                        spawn_mark(line, index, Mark::Leader);
                        line.spawn((
                            PartyLabel(index),
                            Node {
                                // The name takes what the marks leave. `min_width` is the
                                // half that is easy to omit: a flex item's automatic
                                // minimum is its content, so without it a long name would
                                // push the marks off the row instead of being clipped.
                                flex_grow: 1.0,
                                flex_shrink: 1.0,
                                min_width: Val::Px(0.0),
                                overflow: Overflow::clip(),
                                ..default()
                            },
                            Text::new(String::new()),
                            TextFont {
                                font_size: FontSize::Px(LABEL_SIZE),
                                ..default()
                            },
                            TextColor(ALIVE),
                            TextLayout::no_wrap(),
                            TextShadow::default(),
                        ));
                    });
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

/// Spawns one mark's host and the rectangles it is drawn from.
///
/// The host starts at [`Display::None`] rather than [`Visibility::Hidden`]: a hidden node
/// still occupies its place in the flex line, so an absent mark would leave a gap and the
/// name would start in a different column depending on whether a mob happened to be
/// chasing that member.
fn spawn_mark(line: &mut ChildSpawnerCommands<'_>, row: usize, kind: Mark) {
    line.spawn((
        PartyMark { row, kind },
        Node {
            width: Val::Px(MARK_SIZE),
            height: Val::Px(MARK_SIZE),
            flex_shrink: 0.0,
            display: Display::None,
            ..default()
        },
    ))
    .with_children(|mark| {
        for part in kind.parts() {
            mark.spawn((
                PartyMarkPart(row),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(part.left),
                    top: Val::Percent(part.top),
                    width: Val::Percent(part.width),
                    height: Val::Percent(part.height),
                    border_radius: BorderRadius::all(Val::Percent(part.radius)),
                    ..default()
                },
                BackgroundColor(ALIVE),
                UiTransform::from_rotation(Rot2::radians(part.rotation)),
            ));
        }
    });
}

/// What one row is asked to draw this frame.
///
/// The presentation is resolved for every row before anything is written, so the label,
/// the two marks and the bar cannot disagree about which member a row is showing - the
/// marks are separate entities from the text they used to be characters inside, and
/// deriving each from the roster independently is exactly how they would drift.
#[derive(Debug, Clone, PartialEq)]
struct RowPresentation {
    shown: bool,
    text: String,
    colour: Color,
    hunted: bool,
    leader: bool,
    /// Health, maximum health and whether the member is alive, when there is a live entry.
    vitals: Option<(u16, u16, bool)>,
}

impl Default for RowPresentation {
    fn default() -> Self {
        Self {
            shown: false,
            text: String::new(),
            colour: ALIVE,
            hunted: false,
            leader: false,
            vitals: None,
        }
    }
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
    // A mark's host owns a `Node` and a fill owns a `Node`; a mark's rectangle owns a
    // `BackgroundColor` and a fill owns one too. One `Without` on each of the two mark
    // queries is what keeps the three sets of entities disjoint.
    mut marks: Query<(&PartyMark, &mut Node), Without<PartyFill>>,
    mut mark_parts: Query<(&PartyMarkPart, &mut BackgroundColor), Without<PartyFill>>,
    mut fills: Query<(&PartyFill, &mut Node, &mut BackgroundColor)>,
) {
    let shown = present(&view);

    for (row, mut visibility) in &mut rows {
        let next = match shown.get(row.0) {
            Some(row) if row.shown => Visibility::Visible,
            _ => Visibility::Hidden,
        };
        if *visibility != next {
            *visibility = next;
        }
    }
    for (slot, mut text, mut colour) in &mut labels {
        let Some(row) = shown.get(slot.0) else {
            text.0.clear();
            continue;
        };
        if text.0 != row.text {
            text.0.clone_from(&row.text);
        }
        if colour.0 != row.colour {
            colour.0 = row.colour;
        }
    }
    for (mark, mut node) in &mut marks {
        let drawn = shown.get(mark.row).is_some_and(|row| match mark.kind {
            Mark::Hunted => row.hunted,
            Mark::Leader => row.leader,
        });
        let next = if drawn { Display::Flex } else { Display::None };
        if node.display != next {
            node.display = next;
        }
    }
    for (part, mut colour) in &mut mark_parts {
        let next = shown.get(part.0).map_or(ALIVE, |row| row.colour);
        if colour.0 != next {
            colour.0 = next;
        }
    }
    for (slot, mut node, mut colour) in &mut fills {
        let vitals = shown.get(slot.0).and_then(|row| row.vitals);
        let width = Val::Percent(vitals.map_or(0.0, |(health, maximum, _)| {
            health as f32 * 100.0 / maximum as f32
        }));
        if node.width != width {
            node.width = width;
        }
        let next = vitals.map_or(OFFLINE, |(_, _, alive)| if alive { FILL } else { DEAD });
        if colour.0 != next {
            colour.0 = next;
        }
    }
}

/// Resolves what each of the four rows draws, from the newest accepted party snapshot.
///
/// One pass over the roster answering every question a row asks, so the name, the two marks
/// and the bar are one reading of one roster rather than four.
fn present(view: &PartyView<'_>) -> Vec<RowPresentation> {
    let PartyView {
        party,
        appearances,
        vitals,
        session,
        mode,
        snapshots,
    } = view;
    let visible = session.is_some() && matches!(**mode, InputMode::Playing | InputMode::Chat);
    let is_local = |entity_id: u64| {
        session
            .as_deref()
            .is_some_and(|session| session.0.entity_id == entity_id)
    };
    (0..ROW_COUNT)
        .map(|slot| {
            let Some(member) = party.roster.get(slot) else {
                return RowPresentation::default();
            };
            let hunted = snapshots.mob_hunts(member.entity_id);
            // The leader is the first row of the roster, exactly as the crown character
            // said before it was drawn.
            let leader = slot == 0;
            if !member.online {
                return RowPresentation {
                    shown: visible,
                    text: format!("{} | offline", member.name),
                    colour: OFFLINE,
                    hunted,
                    leader,
                    vitals: None,
                };
            }
            let level = if is_local(member.entity_id) {
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
            let live = if is_local(member.entity_id) {
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
            };
            RowPresentation {
                shown: visible,
                text: format!("{name} | Lv {level}"),
                colour: if live.is_some_and(|(_, _, alive)| alive) {
                    ALIVE
                } else {
                    DEAD
                },
                hunted,
                leader,
                vitals: live,
            }
        })
        .collect()
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
            voice_range_blocks: 0.0,
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

    /// Whether each row is drawing `kind`, in row order.
    fn marks(app: &mut App, kind: Mark) -> Vec<bool> {
        let world = app.world_mut();
        let mut query = world.query::<(&PartyMark, &Node)>();
        let mut drawn: Vec<_> = query
            .iter(world)
            .filter(|(mark, _)| mark.kind == kind)
            .map(|(mark, node)| (mark.row, node.display != Display::None))
            .collect();
        drawn.sort_by_key(|(row, _)| *row);
        drawn.into_iter().map(|(_, drawn)| drawn).collect()
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
        assert_eq!(leader.0, "Skald | offline");
        assert_eq!(leader_colour.0, OFFLINE);
        let (_, dead, dead_colour) = labels.iter(world).find(|(slot, _, _)| slot.0 == 1).unwrap();
        assert_eq!(dead.0, "Hunter | Lv 0");
        assert_eq!(dead_colour.0, DEAD);

        // A name still cannot wrap or spill: the line it shares with its marks is the
        // full width of the row and clips, and the name itself gives way inside it.
        let mut lines = world.query::<(&PartyNameLine, &Node)>();
        assert_eq!(lines.iter(world).count(), ROW_COUNT);
        for (_, node) in lines.iter(world) {
            assert_eq!(node.width, Val::Percent(100.0));
            assert_eq!(node.overflow, Overflow::clip());
        }
        let mut label_bounds = world.query::<(&PartyLabel, &Node, &TextLayout)>();
        for (_, node, layout) in label_bounds.iter(world) {
            assert_eq!(node.flex_shrink, 1.0);
            assert_eq!(node.min_width, Val::Px(0.0));
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
        assert_eq!(
            marks(&mut app, Mark::Hunted),
            vec![true, false, false, false]
        );

        accept(&mut app, 2, 11);
        app.update();
        assert_eq!(
            marks(&mut app, Mark::Hunted),
            vec![false, true, false, false]
        );

        accept(&mut app, 3, 0);
        app.update();
        assert_eq!(
            marks(&mut app, Mark::Hunted),
            vec![false; ROW_COUNT],
            "a targetless snapshot left a party mark"
        );
        // The name never carried the mark, so it never lost a column to one either.
        assert_eq!(labels(&mut app)[0], "Local | Lv 0");
    }

    /// The colours the rectangles of row `row` are drawn in.
    fn mark_colours(app: &mut App, row: usize) -> Vec<Color> {
        let world = app.world_mut();
        let mut query = world.query::<(&PartyMarkPart, &BackgroundColor)>();
        query
            .iter(world)
            .filter(|(part, _)| part.0 == row)
            .map(|(_, colour)| colour.0)
            .collect()
    }

    /// The crown was `\u{265b}` inside the leader's own name until #481, and the font this
    /// client draws with has no such glyph — it laid out with zero advance, so the leader
    /// looked exactly like everybody else. It is geometry now, and this holds the two
    /// things the character used to get for free: it is on the leader's row and nobody
    /// else's, and it wears that row's colour rather than one of its own.
    #[test]
    fn the_crown_marks_the_leader_alone_and_takes_that_row_s_colour() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(InputMode::Playing)
            .insert_resource(Party {
                roster: vec![
                    PartyRosterMember {
                        character_id: 90,
                        entity_id: 9,
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
                members: vec![member(11, 80, 80, true)],
            })
            .add_plugins(PartyUiPlugin);
        app.update();

        assert_eq!(
            marks(&mut app, Mark::Leader),
            vec![true, false, false, false]
        );
        assert_eq!(marks(&mut app, Mark::Hunted), vec![false; ROW_COUNT]);
        assert_eq!(labels(&mut app)[0], "Skald | offline");

        // Both marks' rectangles are recoloured, drawn or not, so a mark that appears is
        // already the right colour on the frame it appears.
        let leader = mark_colours(&mut app, 0);
        assert_eq!(leader.len(), CROWN.len() + CROSSED_SWORDS.len());
        assert!(
            leader.iter().all(|colour| *colour == OFFLINE),
            "the offline leader's crown is {leader:?}, not the row's colour"
        );
        assert!(
            mark_colours(&mut app, 1)
                .iter()
                .all(|colour| *colour == ALIVE),
            "a living member's marks left the row's colour behind"
        );
    }

    /// Every rectangle of every mark stays inside the square it is drawn in, so a mark
    /// cannot reach over the name beside it or out of the row.
    #[test]
    fn no_mark_rectangle_leaves_its_own_square() {
        for kind in [Mark::Hunted, Mark::Leader] {
            let parts = kind.parts();
            assert!(!parts.is_empty(), "{kind:?} is drawn from nothing");
            for part in parts {
                // A rotation about the part's own centre reaches half its diagonal from
                // that centre, which is what has to fit rather than the upright box.
                let (half_width, half_height) = (part.width / 2.0, part.height / 2.0);
                let reach = if part.rotation == 0.0 {
                    (half_width, half_height)
                } else {
                    let radius = half_width.hypot(half_height);
                    (radius, radius)
                };
                let (centre_x, centre_y) = (part.left + half_width, part.top + half_height);
                assert!(
                    centre_x - reach.0 >= -0.5
                        && centre_x + reach.0 <= 100.5
                        && centre_y - reach.1 >= -0.5
                        && centre_y + reach.1 <= 100.5,
                    "{kind:?} has a part reaching outside its square: {part:?}"
                );
            }
        }
    }
}
