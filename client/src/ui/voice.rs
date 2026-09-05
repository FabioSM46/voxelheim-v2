//! What the player can see about voice: whether they are being sent, and who they are hearing.
//!
//! **Two lines and nothing else.** The transmit indicator says whether this client is sending
//! right now, and in push to talk — while it is not — it says which key would start. The
//! speaker line names whoever has been heard in the last second. Both are read from resources
//! `audio/` produces; nothing here decides anything, and nothing that decides anything reads
//! this module.
//!
//! **The audience is the one thing this line knows that `audio/` deliberately does not.**
//! `VoiceAudience::Party` on a player who is in no party means *nobody*, and saying so needs
//! the party roster — a server answer, which the HUD already has and `audio/voice.rs` must not
//! read. So the pipeline goes on stamping `Party` on every frame and the server goes on
//! delivering it to an empty set, which is the correct behaviour on both sides: a client that
//! stopped transmitting because its own roster looked empty would be deciding an outcome. What
//! is left is telling the player, and that is this file's whole share of the feature.
//!
//! **It is hidden entirely on a server that relays no voice.** `voice_range_blocks` of zero is
//! `schemas/handshake.fbs`'s "a server that relays no voice at all", so a client there has no
//! voice to indicate — and a key hint for a control that would do nothing is worse than no hint
//! at all.

use bevy::prelude::*;

use crate::audio::{MicrophoneMissing, Speaking, Transmitting, VoiceControls, Voices};
use crate::player::{Appearances, Party};
use crate::settings::{Control, Settings, VoiceAudience, VoiceMode, key_name};

/// Where the pair sits: above the chat log's own lane on the left, out of the way of the
/// vital bars along the bottom.
const VOICE_LEFT: f32 = 12.0;
const VOICE_BOTTOM: f32 = 210.0;

/// The overlay lane the HUD text shares.
const VOICE_LAYER: i32 = 12;

/// The transmit indicator's two colours: sending, and the idle hint.
const SENDING: Color = Color::srgb(0.88, 0.36, 0.30);
const IDLE: Color = Color::srgba(0.72, 0.75, 0.80, 0.75);

/// And the third, for the one state in which the line is a warning rather than a report: a
/// player asking to be heard by a party they are not in.
///
/// The amber `ui/status.rs` answers a server refusal in, deliberately — this is the same kind
/// of sentence, a thing the player asked for that is not going to happen.
const UNHEARD: Color = Color::linear_rgb(1.0, 0.72, 0.25);

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
            .init_resource::<Party>()
            .init_resource::<Voices>()
            .init_resource::<MicrophoneMissing>()
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
    missing: Res<MicrophoneMissing>,
    speaking: Res<Speaking>,
    voices: Res<Voices>,
    appearances: Res<Appearances>,
    party: Res<Party>,
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
    // **The roster and nothing else.** `Party::roster` is the complete authoritative order and
    // includes this character, so a player in a party has a non-empty one and a player in none
    // has an empty one — no count, no threshold, and nothing inferred from `members`, which
    // holds only the *other* online members and is empty for a party of one.
    let in_party = !party.roster.is_empty();
    let (line, colour) = transmit_line(
        controls.mode,
        controls.audience,
        in_party,
        transmitting.0,
        missing.0,
        key,
    );
    for (mut text, mut text_colour) in &mut transmit {
        if text.0 != line {
            text.0.clone_from(&line);
        }
        if text_colour.0 != colour {
            text_colour.0 = colour;
        }
    }

    let heard = speaking.recent(std::time::Instant::now());
    let line = hearing_line(&heard, &voices, &appearances);
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
///
/// **A missing microphone is said before anything else** and without an audience tail: the
/// player is not being heard by a narrower group, they are not being captured at all.
///
/// **The audience is a tail on whatever that says**, not a line of its own: it qualifies the
/// same sentence, and a second line would be a second thing to read for a state that is only
/// ever one word. `Everyone` adds nothing, because it is what a player who never touched the
/// knob has and a HUD that narrated the default would be noise.
fn transmit_line(
    mode: VoiceMode,
    audience: VoiceAudience,
    in_party: bool,
    sending: bool,
    microphone_missing: bool,
    key: &str,
) -> (String, Color) {
    // **First, and before every other state.** A client that cannot open the microphone the
    // player named opens nothing in its place — see `client/AGENTS.md` — so what is left to do
    // is say so, here, where a player who has just held the key and heard no answer is already
    // looking. It takes precedence over the key hint because the key would do nothing, and
    // over the audience because who would have heard it is not the question any more.
    if microphone_missing {
        return ("microphone unavailable".to_owned(), UNHEARD);
    }
    let (line, colour) = match (sending, mode) {
        (true, _) => ("SPEAKING".to_owned(), SENDING),
        (false, VoiceMode::PushToTalk) if !key.is_empty() => {
            (format!("hold [{}] to speak", key.to_uppercase()), IDLE)
        }
        // A control this screen cannot name is a control the player cannot be told about, so
        // the hint says nothing rather than naming a key that is not there.
        (false, VoiceMode::PushToTalk) => ("push to talk".to_owned(), IDLE),
        (false, VoiceMode::VoiceActivation) => ("voice activation".to_owned(), IDLE),
        // Unreachable while the root is shown, since `VoiceControls::live` is false for `Off`.
        // Answered rather than left to a wildcard so a fourth mode has to say what it draws.
        (false, VoiceMode::Off) => (String::new(), IDLE),
    };
    if line.is_empty() {
        return (line, colour);
    }
    match audience {
        VoiceAudience::Everyone => (line, colour),
        VoiceAudience::Party if in_party => (format!("{line} (party)"), colour),
        // **The one state the HUD exists to make visible.** The frames are still going out and
        // the server is still the one deciding they reach nobody; what would be wrong is a
        // player believing they were heard.
        VoiceAudience::Party => (format!("{line} - nobody hears you"), UNHEARD),
    }
}

/// Who is being heard, named the way every other roster names them.
///
/// A speaker the appearance cache has no description for is drawn as their id rather than
/// omitted: hearing somebody the client cannot name is a real state — the description arrives
/// separately from the voice — and dropping the row would make the line disagree with the
/// audio.
///
/// **A speaker nobody can hear is dropped, and that is the same rule read the other way**:
/// this line says who the player is hearing, and they are not hearing somebody mixed at zero.
/// The frames still arrive and `audio/heard.rs` still decodes them — the server decides who may
/// be heard and nothing here overrules it — but naming them would be the line disagreeing with
/// the audio in the opposite direction. The count follows: a crowd of four with two silenced is
/// `+0`, not `+2`.
///
/// **The test is `Voices::audible` and not `!muted`.** A speaker turned all the way down is
/// inaudible without being muted, because `MIN_VOICE` is zero — so filtering on the button
/// rather than on the gain named somebody contributing nothing. Found by review on #941.
fn hearing_line(heard: &[u64], voices: &Voices, appearances: &Appearances) -> String {
    let heard: Vec<u64> = heard
        .iter()
        .copied()
        .filter(|entity_id| voices.audible(*entity_id))
        .collect();
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

    /// The indicator with the audience left where a player who never touched the knob has it.
    fn heard_by_everyone(mode: VoiceMode, sending: bool, key: &str) -> (String, Color) {
        transmit_line(mode, VoiceAudience::Everyone, false, sending, false, key)
    }

    /// **The indicator, in every state it has.** Sending says so; push to talk names the key
    /// that would start it; voice activation names itself, because there is no key to press.
    #[test]
    fn the_indicator_names_the_bound_key_only_where_a_key_would_do_something() {
        assert_eq!(
            heard_by_everyone(VoiceMode::PushToTalk, true, "v").0,
            "SPEAKING"
        );
        assert_eq!(
            heard_by_everyone(VoiceMode::VoiceActivation, true, "v").0,
            "SPEAKING",
            "a mode with no key still says it is sending"
        );
        assert_eq!(
            heard_by_everyone(VoiceMode::PushToTalk, false, "v").0,
            "hold [V] to speak"
        );
        assert_eq!(
            heard_by_everyone(VoiceMode::VoiceActivation, false, "v").0,
            "voice activation",
            "a key hint was drawn for a mode where pressing it does nothing"
        );
        assert_eq!(
            heard_by_everyone(VoiceMode::PushToTalk, false, "").0,
            "push to talk",
            "a hint named a key this screen cannot spell"
        );
        assert_eq!(
            heard_by_everyone(VoiceMode::PushToTalk, true, "v").1,
            SENDING
        );
        assert_eq!(heard_by_everyone(VoiceMode::PushToTalk, false, "v").1, IDLE);
    }

    /// **The audience qualifies the line rather than replacing it, and it has three states.**
    ///
    /// `Everyone` says nothing, because narrating the default is noise. `Party` with a party
    /// says which one. `Party` with no party is the state this whole line exists for: the
    /// client is still transmitting — the server owns who receives, and a client that stopped
    /// on the strength of its own roster would be deciding an outcome — so what is left is
    /// telling the player, in the amber a refusal is answered in.
    #[test]
    fn the_audience_is_a_tail_and_no_party_is_said_out_loud() {
        for (mode, sending, base) in [
            (VoiceMode::PushToTalk, true, "SPEAKING"),
            (VoiceMode::PushToTalk, false, "hold [V] to speak"),
            (VoiceMode::VoiceActivation, false, "voice activation"),
        ] {
            let (plain, plain_colour) =
                transmit_line(mode, VoiceAudience::Everyone, true, sending, false, "v");
            assert_eq!(plain, base, "the widest audience narrated itself");

            let (in_party, colour) =
                transmit_line(mode, VoiceAudience::Party, true, sending, false, "v");
            assert_eq!(in_party, format!("{base} (party)"));
            assert_eq!(
                colour, plain_colour,
                "a party that can hear the player read as a warning"
            );

            let (alone, colour) =
                transmit_line(mode, VoiceAudience::Party, false, sending, false, "v");
            assert_eq!(alone, format!("{base} - nobody hears you"));
            assert_eq!(colour, UNHEARD);
        }

        // `Off` draws nothing at all, and a tail on nothing would be a tail on its own.
        for audience in [VoiceAudience::Everyone, VoiceAudience::Party] {
            for in_party in [true, false] {
                assert_eq!(
                    transmit_line(VoiceMode::Off, audience, in_party, false, false, "v").0,
                    ""
                );
            }
        }
    }

    /// **A missing microphone is said first, in the refusal amber, whatever else is true.**
    ///
    /// This line is the whole of what the client owes a player whose named microphone is not
    /// there — nothing is opened in its place, so the alternative to saying it is a key that
    /// silently does nothing. It takes precedence over the key hint, which would name a
    /// control that cannot work, and over the audience tail, because who *would* have heard
    /// them has stopped being the question.
    #[test]
    fn a_missing_microphone_is_said_before_every_other_state() {
        for mode in [VoiceMode::PushToTalk, VoiceMode::VoiceActivation] {
            for audience in [VoiceAudience::Everyone, VoiceAudience::Party] {
                for in_party in [true, false] {
                    for sending in [true, false] {
                        let (line, colour) =
                            transmit_line(mode, audience, in_party, sending, true, "v");
                        assert_eq!(
                            line, "microphone unavailable",
                            "{mode:?} {audience:?} in_party={in_party} sending={sending}"
                        );
                        assert_eq!(colour, UNHEARD);
                    }
                }
            }
        }

        // And it says nothing of the sort while the microphone is there.
        assert_eq!(
            transmit_line(
                VoiceMode::PushToTalk,
                VoiceAudience::Everyone,
                false,
                false,
                false,
                "v"
            )
            .0,
            "hold [V] to speak"
        );
    }

    /// **And it is true in the assembled HUD**, not only in the line builder: `refresh_voice_hud`
    /// reads `MicrophoneMissing` as a resource of its own, so a state published by `audio/` and
    /// never carried into the node is exactly the half a unit test cannot see.
    #[test]
    fn the_hud_says_the_microphone_is_unavailable() {
        let mut app = App::new();
        app.insert_resource(tuned(VoiceMode::PushToTalk))
            .add_plugins(VoiceUiPlugin);
        app.world_mut().resource_mut::<VoiceControls>().range_blocks = 24.0;
        app.update();

        let line = |app: &mut App| {
            let mut lines = app
                .world_mut()
                .query_filtered::<&Text, With<TransmitLine>>();
            lines
                .iter(app.world())
                .next()
                .map(|text| text.0.clone())
                .unwrap_or_default()
        };
        assert_eq!(line(&mut app), "hold [V] to speak");

        app.world_mut().resource_mut::<MicrophoneMissing>().0 = true;
        app.update();
        assert_eq!(
            line(&mut app),
            "microphone unavailable",
            "the HUD went on offering a key for a microphone that is not there"
        );

        app.world_mut().resource_mut::<MicrophoneMissing>().0 = false;
        app.update();
        assert_eq!(line(&mut app), "hold [V] to speak");
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
            heard_by_everyone(VoiceMode::PushToTalk, false, key).0,
            "hold [B] to speak"
        );
    }

    /// Names come from the same cache every other roster reads, and a speaker with no
    /// description yet is still named rather than dropped.
    #[test]
    fn speakers_are_named_and_an_unknown_one_is_still_shown() {
        assert_eq!(
            hearing_line(&[], &Voices::default(), &Appearances::default()),
            ""
        );

        let described = Appearances::with_player_name(7, "Skald");
        assert_eq!(
            hearing_line(&[7], &Voices::default(), &described),
            "hearing Skald"
        );
        assert_eq!(
            hearing_line(&[7, 9], &Voices::default(), &described),
            "hearing Skald, player 9",
            "a speaker the cache cannot name was dropped from the line"
        );
    }

    /// **A muted speaker is not among the speakers heard, and the count follows.**
    ///
    /// The line says who the player is hearing, and they are not hearing somebody they muted.
    /// The `+n` is asserted with the crowd too, because a count computed before the filter
    /// would say `+2` for two people nobody can hear — the shape a filter added to a `take`
    /// but not to the `len` produces.
    #[test]
    fn a_muted_speaker_is_not_named_and_is_not_counted() {
        let described = Appearances::with_player_name(7, "Skald");
        let mut voices = Voices::default();
        voices.toggle_mute(7);

        assert_eq!(
            hearing_line(&[7], &voices, &described),
            "",
            "the only speaker was muted and the line still named somebody"
        );
        assert_eq!(
            hearing_line(&[7, 9], &voices, &described),
            "hearing player 9"
        );

        // **And turned all the way down, which is inaudible without being muted.** Filtering
        // on the button rather than on the gain named somebody contributing nothing (#941).
        let mut turned_down = Voices::default();
        turned_down.adjust_volume(7, -1_000);
        assert!(
            !turned_down.muted(7),
            "the fixture muted instead of turning down"
        );
        assert_eq!(
            hearing_line(&[7], &turned_down, &described),
            "",
            "a speaker mixed at zero was named as being heard"
        );
        assert_eq!(
            hearing_line(&[7, 9], &turned_down, &described),
            "hearing player 9"
        );

        let heard: Vec<u64> = (1..=7).collect();
        let mut voices = Voices::default();
        for muted in [1, 2] {
            voices.toggle_mute(muted);
        }
        let line = hearing_line(&heard, &voices, &described);
        assert!(!line.contains("Skald"), "a muted speaker was named: {line}");
        assert!(
            line.ends_with("+1"),
            "the overflow counted speakers nobody can hear: {line}"
        );
    }

    /// **And it is true in the assembled HUD, not only in the line builder.** `refresh_voice_hud`
    /// reads `Speaking` and `Voices` as two separate resources, so a filter applied to one of
    /// them and not carried into the node is exactly the half that a unit test on the pure
    /// function cannot see — the shape #924 was.
    #[test]
    fn the_hud_drops_a_muted_speaker_from_its_line() {
        let mut app = App::new();
        app.insert_resource(tuned(VoiceMode::PushToTalk))
            .add_plugins(VoiceUiPlugin);
        app.world_mut().resource_mut::<VoiceControls>().range_blocks = 24.0;
        app.world_mut()
            .resource_mut::<Speaking>()
            .heard_for_test(7, Instant::now());
        app.update();

        let line = |app: &mut App| {
            let mut heard = app.world_mut().query_filtered::<&Text, With<HearingLine>>();
            heard
                .iter(app.world())
                .next()
                .map(|text| text.0.clone())
                .unwrap_or_default()
        };
        assert_eq!(line(&mut app), "hearing player 7");

        app.world_mut().resource_mut::<Voices>().toggle_mute(7);
        app.update();
        assert_eq!(
            line(&mut app),
            "",
            "the HUD went on naming a speaker the player had muted"
        );
    }

    /// **A crowd is counted, not drawn.**
    /// **A crowd is counted, not drawn.** How many people are within voice range is the
    /// server's answer, and a line that grew with it would run off the screen.
    #[test]
    fn more_speakers_than_fit_are_counted() {
        let described = Appearances::with_player_name(1, "Skald");
        let heard: Vec<u64> = (1..=7).collect();
        let line = hearing_line(&heard, &Voices::default(), &described);
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
