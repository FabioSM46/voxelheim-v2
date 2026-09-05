//! What the player can see about voice: whether they are being sent, and who they are hearing.
//!
//! **Two lines and nothing else.** The transmit indicator says whether this client is sending
//! right now, and in push to talk — while it is not — it says which key would start. The
//! speaker line names whoever has been heard in the last second. Both are read from resources
//! `audio/` produces; nothing here decides anything, and nothing that decides anything reads
//! this module.
//!
//! **It is hidden entirely on a server that relays no voice.** `voice_range_blocks` of zero is
//! `schemas/handshake.fbs`'s "a server that relays no voice at all", so a client there has no
//! voice to indicate — and a key hint for a control that would do nothing is worse than no hint
//! at all.

use bevy::prelude::*;

use crate::audio::{Speaking, Transmitting, VoiceControls};
use crate::player::Appearances;
use crate::settings::{Control, Settings, VoiceMode, key_name};

/// Where the pair sits: above the chat log's own lane on the left, out of the way of the
/// vital bars along the bottom.
const VOICE_LEFT: f32 = 12.0;
const VOICE_BOTTOM: f32 = 210.0;

/// The overlay lane the HUD text shares.
const VOICE_LAYER: i32 = 12;

/// The transmit indicator's two colours: sending, and the idle hint.
const SENDING: Color = Color::srgb(0.88, 0.36, 0.30);
const IDLE: Color = Color::srgba(0.72, 0.75, 0.80, 0.75);

/// What the speaker names are drawn in.
const HEARING: Color = Color::srgb(0.72, 0.84, 0.94);

const VOICE_FONT_SIZE: f32 = 14.0;

/// The most speakers named at once.
///
/// **A bound on a list the world fills.** How many people are within voice range is the
/// server's answer and can be large; a line that grew with it would run off the screen. The
/// rest are counted rather than dropped silently.
const NAMED_SPEAKERS: usize = 4;

/// The indicator line.
#[derive(Component)]
struct TransmitLine;

/// The speakers line.
#[derive(Component)]
struct HearingLine;

/// The pair's root, hidden whole when the server relays no voice.
#[derive(Component)]
struct VoiceHud;

pub(super) struct VoiceUiPlugin;

impl Plugin for VoiceUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VoiceControls>()
            .init_resource::<Transmitting>()
            .init_resource::<Speaking>()
            .init_resource::<Appearances>()
            .add_systems(Startup, spawn_voice_hud)
            .add_systems(Update, refresh_voice_hud);
    }
}

fn spawn_voice_hud(mut commands: Commands) {
    commands
        .spawn((
            VoiceHud,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(VOICE_LEFT),
                bottom: Val::Px(VOICE_BOTTOM),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                // Nothing at all until a session says voice is carried, not merely nothing
                // visible: `refresh_voice_hud` answers that question fresh every frame.
                display: Display::None,
                ..default()
            },
            GlobalZIndex(VOICE_LAYER),
        ))
        .with_children(|root| {
            let line = |colour: Color| {
                (
                    Node::default(),
                    Text::new(String::new()),
                    TextFont {
                        font_size: FontSize::Px(VOICE_FONT_SIZE),
                        ..default()
                    },
                    TextColor(colour),
                    TextLayout::no_wrap(),
                    TextShadow::default(),
                )
            };
            root.spawn((TransmitLine, line(SENDING)));
            root.spawn((HearingLine, line(HEARING)));
        });
}

/// The pair, from the newest answers `audio/` produced.
// Eight, and each is one of the four answers this line is made of or one of the two nodes it
// writes. Splitting it would mean reading `VoiceControls` twice and deciding twice from it,
// which is the shape `audio/voice.rs`'s `speak` avoids for the same reason.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn refresh_voice_hud(
    controls: Res<VoiceControls>,
    transmitting: Res<Transmitting>,
    speaking: Res<Speaking>,
    appearances: Res<Appearances>,
    settings: Option<Res<Settings>>,
    mut roots: Query<&mut Node, With<VoiceHud>>,
    mut transmit: Query<(&mut Text, &mut TextColor), (With<TransmitLine>, Without<HearingLine>)>,
    mut hearing: Query<&mut Text, (With<HearingLine>, Without<TransmitLine>)>,
) {
    let display = if controls.live() {
        Display::Flex
    } else {
        Display::None
    };
    for mut root in &mut roots {
        if root.display != display {
            root.display = display;
        }
    }
    if display == Display::None {
        return;
    }

    let key = settings
        .as_deref()
        .map(|settings| settings.bindings().key(Control::Talk))
        .and_then(key_name)
        .unwrap_or("");
    let (line, colour) = transmit_line(controls.mode, transmitting.0, key);
    for (mut text, mut text_colour) in &mut transmit {
        if text.0 != line {
            text.0.clone_from(&line);
        }
        if text_colour.0 != colour {
            text_colour.0 = colour;
        }
    }

    let heard = speaking.recent(std::time::Instant::now());
    let line = hearing_line(&heard, &appearances);
    for mut text in &mut hearing {
        if text.0 != line {
            text.0.clone_from(&line);
        }
    }
}

/// What the indicator says, and in which colour.
///
/// **The key hint is push to talk's alone**, because it is the only mode in which pressing
/// anything starts a transmission. Voice activation waits for a level, and a hint naming a key
/// there would be telling the player about a control that does nothing.
fn transmit_line(mode: VoiceMode, sending: bool, key: &str) -> (String, Color) {
    if sending {
        return ("SPEAKING".to_owned(), SENDING);
    }
    match mode {
        VoiceMode::PushToTalk if !key.is_empty() => {
            (format!("hold [{}] to speak", key.to_uppercase()), IDLE)
        }
        // A control this screen cannot name is a control the player cannot be told about, so
        // the hint says nothing rather than naming a key that is not there.
        VoiceMode::PushToTalk => ("push to talk".to_owned(), IDLE),
        VoiceMode::VoiceActivation => ("voice activation".to_owned(), IDLE),
        // Unreachable while the root is shown, since `VoiceControls::live` is false for `Off`.
        // Answered rather than left to a wildcard so a fourth mode has to say what it draws.
        VoiceMode::Off => (String::new(), IDLE),
    }
}

/// Who is being heard, named the way every other roster names them.
///
/// A speaker the appearance cache has no description for is drawn as their id rather than
/// omitted: hearing somebody the client cannot name is a real state — the description arrives
/// separately from the voice — and dropping the row would make the line disagree with the
/// audio.
fn hearing_line(heard: &[u64], appearances: &Appearances) -> String {
    if heard.is_empty() {
        return String::new();
    }
    let mut names: Vec<String> = heard
        .iter()
        .take(NAMED_SPEAKERS)
        .map(|entity_id| {
            appearances
                .name(*entity_id)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format!("player {entity_id}"))
        })
        .collect();
    if heard.len() > NAMED_SPEAKERS {
        names.push(format!("+{}", heard.len() - NAMED_SPEAKERS));
    }
    format!("hearing {}", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::SPEAKING_FOR;
    use crate::settings::Knob;
    use std::time::{Duration, Instant};

    fn tuned(mode: VoiceMode) -> Settings {
        let mut settings = Settings::default();
        while settings.voice_mode() != mode {
            settings.adjust(Knob::VoiceMode, if mode == VoiceMode::Off { -1 } else { 1 });
        }
        settings
    }

    /// **The indicator, in every state it has.** Sending says so; push to talk names the key
    /// that would start it; voice activation names itself, because there is no key to press.
    #[test]
    fn the_indicator_names_the_bound_key_only_where_a_key_would_do_something() {
        assert_eq!(
            transmit_line(VoiceMode::PushToTalk, true, "v").0,
            "SPEAKING"
        );
        assert_eq!(
            transmit_line(VoiceMode::VoiceActivation, true, "v").0,
            "SPEAKING",
            "a mode with no key still says it is sending"
        );
        assert_eq!(
            transmit_line(VoiceMode::PushToTalk, false, "v").0,
            "hold [V] to speak"
        );
        assert_eq!(
            transmit_line(VoiceMode::VoiceActivation, false, "v").0,
            "voice activation",
            "a key hint was drawn for a mode where pressing it does nothing"
        );
        assert_eq!(
            transmit_line(VoiceMode::PushToTalk, false, "").0,
            "push to talk",
            "a hint named a key this screen cannot spell"
        );
        assert_eq!(transmit_line(VoiceMode::PushToTalk, true, "v").1, SENDING);
        assert_eq!(transmit_line(VoiceMode::PushToTalk, false, "v").1, IDLE);
    }

    /// **The key is the one the player bound**, taken from the bindings rather than from the
    /// default this client ships with.
    #[test]
    fn the_hint_follows_a_rebinding() {
        let mut settings = tuned(VoiceMode::PushToTalk);
        settings
            .rebind(Control::Talk, KeyCode::KeyB)
            .expect("b is free");
        let key = key_name(settings.bindings().key(Control::Talk)).expect("a name");
        assert_eq!(
            transmit_line(VoiceMode::PushToTalk, false, key).0,
            "hold [B] to speak"
        );
    }

    /// Names come from the same cache every other roster reads, and a speaker with no
    /// description yet is still named rather than dropped.
    #[test]
    fn speakers_are_named_and_an_unknown_one_is_still_shown() {
        assert_eq!(hearing_line(&[], &Appearances::default()), "");

        let described = Appearances::with_player_name(7, "Skald");
        assert_eq!(hearing_line(&[7], &described), "hearing Skald");
        assert_eq!(
            hearing_line(&[7, 9], &described),
            "hearing Skald, player 9",
            "a speaker the cache cannot name was dropped from the line"
        );
    }

    /// **A crowd is counted, not drawn.** How many people are within voice range is the
    /// server's answer, and a line that grew with it would run off the screen.
    #[test]
    fn more_speakers_than_fit_are_counted() {
        let described = Appearances::with_player_name(1, "Skald");
        let heard: Vec<u64> = (1..=7).collect();
        let line = hearing_line(&heard, &described);
        assert!(line.starts_with("hearing Skald,"), "{line}");
        assert!(line.ends_with("+3"), "{line}");
        assert_eq!(
            line.matches(", ").count(),
            NAMED_SPEAKERS,
            "more than {NAMED_SPEAKERS} names were drawn: {line}"
        );
    }

    /// The whole pair, end to end: hidden on a server that relays no voice, shown on one that
    /// does, and the two lines saying what the resources say.
    #[test]
    fn the_hud_is_hidden_on_a_server_that_relays_no_voice() {
        let mut app = App::new();
        app.insert_resource(tuned(VoiceMode::PushToTalk))
            .add_plugins(VoiceUiPlugin);
        app.update();

        let shown = |app: &mut App| {
            let mut roots = app.world_mut().query_filtered::<&Node, With<VoiceHud>>();
            roots
                .iter(app.world())
                .all(|node| node.display == Display::Flex)
        };
        assert!(!shown(&mut app), "the HUD was up with no session");

        app.world_mut().resource_mut::<VoiceControls>().range_blocks = 24.0;
        app.update();
        assert!(shown(&mut app), "the HUD stayed down on a voice server");

        let mut lines = app
            .world_mut()
            .query_filtered::<&Text, With<TransmitLine>>();
        assert_eq!(
            lines.iter(app.world()).next().map(|text| text.0.clone()),
            Some("hold [V] to speak".to_owned())
        );

        app.world_mut().resource_mut::<Transmitting>().0 = true;
        app.world_mut()
            .resource_mut::<Speaking>()
            .heard_for_test(7, Instant::now());
        app.update();
        let mut lines = app
            .world_mut()
            .query_filtered::<&Text, With<TransmitLine>>();
        assert_eq!(
            lines.iter(app.world()).next().map(|text| text.0.clone()),
            Some("SPEAKING".to_owned())
        );
        let mut heard = app.world_mut().query_filtered::<&Text, With<HearingLine>>();
        assert_eq!(
            heard.iter(app.world()).next().map(|text| text.0.clone()),
            Some("hearing player 7".to_owned())
        );

        // And it goes away again when the server that carried it does.
        app.world_mut().resource_mut::<VoiceControls>().range_blocks = 0.0;
        app.update();
        assert!(!shown(&mut app), "the HUD outlived the session");
        assert!(SPEAKING_FOR > Duration::from_millis(500));
    }
}
