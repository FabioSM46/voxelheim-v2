//! The crafting station panel and the world prompt that teaches the key opening it.
//!
//! Both are pictures of `player::station` and neither originates anything. The prompt says
//! which station the interact key would open this frame; the panel says which station is
//! open. What a player may make there — and whether it works — is the recipe mirror's and
//! the server's respectively.

use bevy::prelude::*;

use crate::net::Session;
use crate::player::{InputMode, StationHint, StationWindow, station_title};

const WIDTH: f32 = 430.0;

/// The label the prompt names: the *default* interact binding, for the reason
/// `ui/loot.rs`'s take-all line gives — a hint is presentation, and this module does not
/// read `Settings`.
const USE_KEY: &str = "F";

/// The loot window's hint ink, so the two lines that teach the interact key read alike.
const HINT: Color = Color::srgb(0.62, 0.62, 0.66);

/// How far under the crosshair's centre the prompt sits, in logical pixels: clear of the
/// crosshair and its mining ring, near enough to read as being about what is aimed at.
const PROMPT_OFFSET: f32 = 44.0;

#[derive(Component)]
struct StationRoot;

#[derive(Component)]
struct StationTitle;

#[derive(Component)]
struct StationPrompt;

pub(super) struct StationUiPlugin;

impl Plugin for StationUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StationWindow>()
            .init_resource::<StationHint>()
            .init_resource::<InputMode>()
            .add_systems(Startup, spawn_station_ui)
            .add_systems(Update, (show_panel, show_prompt));
    }
}

fn spawn_station_ui(mut commands: Commands) {
    commands
        .spawn((
            StationRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(WIDTH),
                max_height: Val::Percent(80.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            },
            UiTransform::from_translation(Val2::percent(-50.0, -50.0)),
            // The loot, vendor and prompt frame, exactly.
            BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.96)),
            GlobalZIndex(30),
            Visibility::Hidden,
        ))
        .with_child((
            StationTitle,
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(22.0),
                ..default()
            },
            TextColor(Color::WHITE),
        ));

    commands.spawn((
        StationPrompt,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(14.0),
            ..default()
        },
        TextColor(HINT),
        // Over the world rather than on a frame, so it needs the shadow the loot window's
        // dark panel otherwise provides.
        TextShadow::default(),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            ..default()
        },
        UiTransform::from_translation(Val2::new(Val::Percent(-50.0), Val::Px(PROMPT_OFFSET))),
        GlobalZIndex(10),
        Visibility::Hidden,
    ));
}

/// Titles the panel by its station, and shows it while the station mode owns the screen.
fn show_panel(
    window: Res<StationWindow>,
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Visibility, With<StationRoot>>,
    mut titles: Query<&mut Text, With<StationTitle>>,
) {
    let open = window
        .station()
        .filter(|_| session.is_some() && *mode == InputMode::Station);
    if let Some(kind) = open {
        for mut title in &mut titles {
            if title.0 != station_title(kind) {
                title.0 = station_title(kind).to_owned();
            }
        }
    }
    let next = if open.is_some() {
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

/// "F - use forge" under the crosshair while a station would take the key.
///
/// ASCII with a hyphen, for the reason the take-all line gives: Bevy's default font has no
/// em dash, and a hint that draws a gap where its only punctuation should be hides the one
/// thing it is for.
fn show_prompt(
    hint: Res<StationHint>,
    session: Option<Res<Session>>,
    mut prompts: Query<(&mut Text, &mut Visibility), With<StationPrompt>>,
) {
    let shown = hint.0.filter(|_| session.is_some());
    for (mut text, mut visibility) in &mut prompts {
        if let Some(kind) = shown {
            let line = prompt_line(station_title(kind));
            if text.0 != line {
                text.0 = line;
            }
        }
        let next = if shown.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != next {
            *visibility = next;
        }
    }
}

fn prompt_line(title: &str) -> String {
    format!("{USE_KEY} - use {}", title.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, SessionParams, StructureKind};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Session(SessionParams {
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
                voice_range_blocks: 0.0,
            }))
            .add_plugins(StationUiPlugin);
        app.update();
        app
    }

    fn prompt(app: &mut App) -> (String, Visibility) {
        let world = app.world_mut();
        let (text, visibility) = world
            .query_filtered::<(&Text, &Visibility), With<StationPrompt>>()
            .single(world)
            .expect("one station prompt");
        (text.0.clone(), *visibility)
    }

    #[test]
    fn the_prompt_names_the_station_in_ascii_and_hides_without_one() {
        let mut app = app();
        assert_eq!(prompt(&mut app).1, Visibility::Hidden);

        app.insert_resource(StationHint(Some(StructureKind::LeatherBench)));
        app.update();
        let (line, visibility) = prompt(&mut app);
        assert_eq!(line, "F - use leather bench");
        assert!(line.is_ascii(), "the prompt carries an undrawable glyph");
        assert_eq!(visibility, Visibility::Visible);

        app.insert_resource(StationHint(None));
        app.update();
        assert_eq!(prompt(&mut app).1, Visibility::Hidden);
    }

    #[test]
    fn the_panel_is_titled_by_its_station_and_shown_only_in_the_station_mode() {
        let mut app = app();
        app.insert_resource(StationWindow::at(900, StructureKind::ArmourBench));
        app.update();
        let hidden = {
            let world = app.world_mut();
            *world
                .query_filtered::<&Visibility, With<StationRoot>>()
                .single(world)
                .expect("one station panel")
        };
        assert_eq!(
            hidden,
            Visibility::Hidden,
            "a panel opened outside its mode"
        );

        app.insert_resource(InputMode::Station);
        app.update();
        let world = app.world_mut();
        let visibility = *world
            .query_filtered::<&Visibility, With<StationRoot>>()
            .single(world)
            .expect("one station panel");
        assert_eq!(visibility, Visibility::Visible);
        let title = world
            .query_filtered::<&Text, With<StationTitle>>()
            .single(world)
            .expect("one title");
        assert_eq!(title.0, "Armour bench");
    }
}
