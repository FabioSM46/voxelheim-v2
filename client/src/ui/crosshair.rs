//! A high-contrast crosshair and authoritative mining ring at screen centre.

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::prelude::*;

use crate::net::Session;
use crate::player::{ApplyMiningFeedback, InputMode, MiningFeedback};
use crate::world::palette;

const FRAME_EDGE: f32 = 48.0;
const CROSSHAIR_OFFSET: f32 = 12.0;
const RING_SEGMENTS: u8 = 16;
const RING_RADIUS: f32 = 20.0;
const RING_DOT: f32 = 3.0;

pub(super) struct CrosshairPlugin;

impl Plugin for CrosshairPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MiningFeedback>()
            .add_systems(Startup, spawn_crosshair)
            .add_systems(
                Update,
                (
                    show_crosshair,
                    show_mining_progress.after(ApplyMiningFeedback),
                ),
            );
    }
}

#[derive(Component)]
struct CrosshairRoot;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum CrosshairPart {
    DarkHorizontal,
    LightHorizontal,
    DarkVertical,
    LightVertical,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct MiningRingSegment(u8);

fn spawn_crosshair(mut commands: Commands) {
    commands
        .spawn((
            CrosshairRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            Visibility::Hidden,
            GlobalZIndex(10),
        ))
        .with_children(|root| {
            root.spawn(Node {
                width: Val::Px(FRAME_EDGE),
                height: Val::Px(FRAME_EDGE),
                ..default()
            })
            .with_children(|frame| {
                for (part, node, colour) in [
                    (
                        CrosshairPart::DarkHorizontal,
                        bar(CROSSHAIR_OFFSET, CROSSHAIR_OFFSET + 10.0, 24.0, 4.0),
                        Color::BLACK,
                    ),
                    (
                        CrosshairPart::LightHorizontal,
                        bar(CROSSHAIR_OFFSET + 2.0, CROSSHAIR_OFFSET + 11.0, 20.0, 2.0),
                        Color::WHITE,
                    ),
                    (
                        CrosshairPart::DarkVertical,
                        bar(CROSSHAIR_OFFSET + 10.0, CROSSHAIR_OFFSET, 4.0, 24.0),
                        Color::BLACK,
                    ),
                    (
                        CrosshairPart::LightVertical,
                        bar(CROSSHAIR_OFFSET + 11.0, CROSSHAIR_OFFSET + 2.0, 2.0, 20.0),
                        Color::WHITE,
                    ),
                ] {
                    frame.spawn((part, node, BackgroundColor(colour)));
                }

                let [r, g, b, _] = palette::linear_rgba(palette::SNOW);
                for segment in 0..RING_SEGMENTS {
                    let angle = -FRAC_PI_2 + TAU * f32::from(segment) / f32::from(RING_SEGMENTS);
                    let centre = Vec2::splat(FRAME_EDGE / 2.0)
                        + Vec2::new(angle.cos(), angle.sin()) * RING_RADIUS;
                    frame.spawn((
                        MiningRingSegment(segment),
                        bar(
                            centre.x - RING_DOT / 2.0,
                            centre.y - RING_DOT / 2.0,
                            RING_DOT,
                            RING_DOT,
                        ),
                        BackgroundColor(Color::linear_rgb(r, g, b)),
                        Visibility::Hidden,
                    ));
                }
            });
        });
}

fn show_mining_progress(
    feedback: Res<MiningFeedback>,
    mut segments: Query<(&MiningRingSegment, &mut Visibility)>,
) {
    if !feedback.is_changed() {
        return;
    }
    let filled = filled_segments(feedback.progress());
    for (segment, mut visibility) in &mut segments {
        let next = if segment.0 < filled {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != next {
            *visibility = next;
        }
    }
}

fn filled_segments(progress: u8) -> u8 {
    if progress == 0 {
        return 0;
    }
    let filled = (u16::from(progress) * u16::from(RING_SEGMENTS)).div_ceil(u16::from(u8::MAX));
    filled as u8
}

fn bar(left: f32, top: f32, width: f32, height: f32) -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(left),
        top: Val::Px(top),
        width: Val::Px(width),
        height: Val::Px(height),
        ..default()
    }
}

fn show_crosshair(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Visibility, With<CrosshairRoot>>,
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
    use super::*;
    use crate::net::SessionParams;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 4,
            hotbar_slots: 4,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn root_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<CrosshairRoot>>();
        *query.single(world).expect("one crosshair root")
    }

    #[test]
    fn black_outline_and_white_core_share_the_window_centre() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<InputMode>()
            .insert_resource(session())
            .add_plugins(CrosshairPlugin);
        app.update();

        let world = app.world_mut();
        let mut parts = world.query::<(&CrosshairPart, &BackgroundColor)>();
        let found: Vec<(CrosshairPart, Color)> = parts
            .iter(world)
            .map(|(part, colour)| (*part, colour.0))
            .collect();
        assert_eq!(found.len(), 4);
        assert_eq!(
            found
                .iter()
                .filter(|(_, colour)| *colour == Color::BLACK)
                .count(),
            2
        );
        assert_eq!(
            found
                .iter()
                .filter(|(_, colour)| *colour == Color::WHITE)
                .count(),
            2
        );
    }

    #[test]
    fn authoritative_progress_fills_the_ring_and_zero_clears_it() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<InputMode>()
            .insert_resource(session())
            .insert_resource(MiningFeedback::for_test(128))
            .add_plugins(CrosshairPlugin);
        app.update();

        let visible = |app: &mut App| {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Visibility, With<MiningRingSegment>>();
            query
                .iter(world)
                .filter(|visibility| **visibility == Visibility::Visible)
                .count()
        };
        assert_eq!(visible(&mut app), usize::from(filled_segments(128)));

        app.insert_resource(MiningFeedback::default());
        app.update();
        assert_eq!(visible(&mut app), 0);
    }

    #[test]
    fn inventory_and_menu_hide_the_crosshair_and_its_ring() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<InputMode>()
            .insert_resource(session())
            .insert_resource(MiningFeedback::for_test(u8::MAX))
            .add_plugins(CrosshairPlugin);
        app.update();
        assert_eq!(root_visibility(&mut app), Visibility::Visible);

        for mode in [InputMode::Inventory, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(
                root_visibility(&mut app),
                Visibility::Hidden,
                "mode {mode:?}"
            );
        }
    }
}
