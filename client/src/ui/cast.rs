use bevy::prelude::*;

use crate::net::{CastKind, Session};
use crate::player::{MountFeedback, SnapshotBuffer};

#[derive(Component)]
struct CastRoot;

#[derive(Component)]
struct CastFill;

#[derive(Component)]
struct CastLabel;

#[derive(Component)]
struct MountFeedbackText;

pub(super) struct CastUiPlugin;

impl Plugin for CastUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SnapshotBuffer>()
            .init_resource::<MountFeedback>()
            .add_systems(Startup, spawn_cast_ui)
            .add_systems(Update, (refresh_cast, refresh_feedback));
    }
}

fn spawn_cast_ui(mut commands: Commands) {
    commands
        .spawn((
            CastRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(35.0),
                bottom: Val::Px(92.0),
                width: Val::Percent(30.0),
                display: Display::None,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(5.0),
                ..default()
            },
            GlobalZIndex(14),
        ))
        .with_children(|root| {
            root.spawn((
                CastLabel,
                Text::new("Calling mount"),
                TextFont {
                    font_size: FontSize::Px(16.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            root.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(12.0),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.02, 0.025, 0.035, 0.92)),
            ))
            .with_child((
                CastFill,
                Node {
                    width: Val::Percent(0.0),
                    height: Val::Percent(100.0),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.83, 0.62, 0.22)),
            ));
        });
    commands.spawn((
        MountFeedbackText,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(18.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(30.0),
            bottom: Val::Px(62.0),
            width: Val::Percent(40.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(14),
    ));
}

fn refresh_cast(
    snapshots: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Node, With<CastRoot>>,
    mut fills: Query<&mut Node, (With<CastFill>, Without<CastRoot>)>,
    mut labels: Query<&mut Text, With<CastLabel>>,
) {
    let state = session.and_then(|_| snapshots.self_cast());
    for mut root in &mut roots {
        root.display = if state.is_some() {
            Display::Flex
        } else {
            Display::None
        };
    }
    let Some(state) = state else {
        return;
    };
    for mut fill in &mut fills {
        fill.width = Val::Percent(f32::from(state.progress) * 100.0 / 255.0);
    }
    let label = match state.kind {
        CastKind::Mount => "Calling mount",
    };
    for mut text in &mut labels {
        text.0 = format!("{label}  {}%", u16::from(state.progress) * 100 / 255);
    }
}

fn refresh_feedback(
    feedback: Res<MountFeedback>,
    mut labels: Query<&mut Text, With<MountFeedbackText>>,
) {
    if !feedback.is_changed() {
        return;
    }
    for mut text in &mut labels {
        text.0 = feedback.line().unwrap_or_default().to_owned();
    }
}
