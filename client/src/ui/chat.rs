//! The transient world-chat log and the one-line text entry surface.
//!
//! Received text is presentation only: this module bounds it for layout and never parses
//! it as a command or trusts its sender name as identity. The five party commands become
//! typed requests; every other slash-prefixed line reaches the authoritative server as
//! chat-carried command input.

use std::collections::VecDeque;
use std::time::Duration;

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::time::Real;

use crate::net::{
    ChatEntry, ChatInbox, ChatRequest, DrainNetwork, Outbound, PartyAction, PartyRequest, Sent,
    Session, encode_chat_request, encode_party_request,
};
use crate::player::{ApplyInputMode, ApplySnapshots, InputMode, PartyLogInbox};

use super::text_input::{TextEdit, apply_key};
use super::{PlayerMessage, PlayerMessageKind, PublishPlayerMessages, set_mode};

const LINE_COUNT: usize = 8;
const LINE_LIFETIME: Duration = Duration::from_secs(12);
const DRAFT_LIMIT_BYTES: usize = 256;
const SENDER_CHARACTERS: usize = 48;
const MESSAGE_CHARACTERS: usize = 256;
// A reserved display hint rather than identity. A player can have this name too;
// avoiding that ambiguity would require a new schema member, which development-only
// command feedback does not justify.
const COMMAND_SENDER_NAME: &str = "Server";
const FONT_SIZE: FontSize = FontSize::Px(17.0);
const LEFT: f32 = 16.0;
const INPUT_BOTTOM: f32 = 44.0;
const LOG_BOTTOM: f32 = 70.0;

#[derive(Resource, Debug, Default, PartialEq, Eq)]
struct ChatLine(String);

/// The one submitted line this process can recall.
///
/// This stays beside the draft rather than in the shared text-input helper: remembering a
/// submission is chat behaviour, and the map's note field must keep treating arrows as no-op
/// keys. It is deliberately a single optional line rather than a growing session log.
#[derive(Resource, Debug, Default, PartialEq, Eq)]
struct ChatHistory(Option<String>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogKind {
    Player,
    Highlight,
    System(PlayerMessageKind),
}

#[derive(Debug, Clone, PartialEq)]
struct LogLine {
    text: String,
    added: Duration,
    kind: LogKind,
}

#[derive(Resource, Debug, Default)]
pub(super) struct ChatLog(VecDeque<LogLine>);

impl ChatLog {
    fn push(&mut self, text: String, now: Duration) {
        self.push_kind(text, now, LogKind::Player);
    }

    fn push_kind(&mut self, text: String, now: Duration, kind: LogKind) {
        if self.0.len() == LINE_COUNT {
            self.0.pop_front();
        }
        self.0.push_back(LogLine {
            text,
            added: now,
            kind,
        });
    }

    fn push_highlighted(&mut self, text: String, now: Duration) {
        self.push_kind(text, now, LogKind::Highlight);
    }
}

#[derive(Component)]
struct ChatText(usize);

#[derive(Component)]
struct ChatInput;

pub(super) struct ChatUiPlugin;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct RenderChat;

impl Plugin for ChatUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChatLine>()
            .init_resource::<ChatHistory>()
            .init_resource::<ChatLog>()
            .init_resource::<ChatInbox>()
            .init_resource::<PartyLogInbox>()
            .add_message::<KeyboardInput>()
            .add_message::<PlayerMessage>()
            .add_systems(Startup, spawn_chat)
            .add_systems(
                Update,
                (
                    ingest_server_lines.after(DrainNetwork),
                    ingest_party_lines
                        .after(ApplySnapshots)
                        .in_set(PublishPlayerMessages),
                    capture_chat.after(ApplyInputMode),
                    ingest_player_messages.after(PublishPlayerMessages),
                    render_chat.in_set(RenderChat),
                )
                    .chain(),
            );
    }
}

fn spawn_chat(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(LEFT),
                bottom: Val::Px(LOG_BOTTOM),
                width: Val::Percent(38.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            GlobalZIndex(14),
        ))
        .with_children(|root| {
            for index in 0..LINE_COUNT {
                root.spawn((
                    ChatText(index),
                    Text::new(String::new()),
                    TextFont {
                        font_size: FONT_SIZE,
                        ..default()
                    },
                    TextColor(Color::NONE),
                    TextShadow::default(),
                ));
            }
        });

    commands.spawn((
        ChatInput,
        Text::new(String::new()),
        TextFont {
            font_size: FONT_SIZE,
            ..default()
        },
        TextColor(Color::WHITE),
        TextShadow::default(),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(LEFT),
            bottom: Val::Px(INPUT_BOTTOM),
            width: Val::Percent(38.0),
            ..default()
        },
        GlobalZIndex(14),
    ));
}

fn ingest_server_lines(
    time: Res<Time<Real>>,
    mut chat: ResMut<ChatInbox>,
    mut log: ResMut<ChatLog>,
) {
    let now = time.elapsed();
    for entry in chat.take() {
        match entry {
            ChatEntry::Message(message) => {
                let text = bounded_display(&message.text, MESSAGE_CHARACTERS);
                if message.sender_name == COMMAND_SENDER_NAME {
                    push_player_message(
                        &mut log,
                        &PlayerMessage::new(PlayerMessageKind::Server, text),
                        now,
                    );
                } else {
                    let sender = bounded_display(&message.sender_name, SENDER_CHARACTERS);
                    log.push(format!("{sender}: {text}"), now);
                }
            }
            ChatEntry::PartyInvite(invite) => {
                let sender = bounded_display(&invite.from_name, SENDER_CHARACTERS);
                log.push_highlighted(
                    format!("{sender} invites you to a party - /accept or /decline"),
                    now,
                );
            }
        }
    }
}

fn ingest_party_lines(
    time: Res<Time<Real>>,
    session: Option<Res<Session>>,
    mut inbox: ResMut<PartyLogInbox>,
    mut log: ResMut<ChatLog>,
) {
    if session.is_none() {
        drop(inbox.take());
        return;
    }
    for line in inbox.take() {
        log.push(line, time.elapsed());
    }
}

fn ingest_player_messages(
    time: Res<Time<Real>>,
    mut messages: MessageReader<PlayerMessage>,
    mut log: ResMut<ChatLog>,
) {
    let now = time.elapsed();
    for message in messages.read() {
        push_player_message(&mut log, message, now);
    }
}

fn push_player_message(log: &mut ChatLog, message: &PlayerMessage, now: Duration) {
    let text = bounded_display(&message.text, MESSAGE_CHARACTERS);
    log.push_kind(
        format!("{} {text}", message_tag(message.kind)),
        now,
        LogKind::System(message.kind),
    );
}

const fn message_tag(kind: PlayerMessageKind) -> &'static str {
    match kind {
        PlayerMessageKind::Server => "[SERVER]",
        PlayerMessageKind::Info => "[INFO]",
        PlayerMessageKind::Warn => "[WARN]",
        PlayerMessageKind::Error => "[ERROR]",
    }
}

fn message_colour(kind: LogKind, alpha: f32) -> Color {
    let (red, green, blue) = match kind {
        LogKind::Player => (1.0, 1.0, 1.0),
        LogKind::Highlight => (1.0, 0.72, 0.25),
        LogKind::System(PlayerMessageKind::Server) => (1.0, 0.72, 0.25),
        LogKind::System(PlayerMessageKind::Info) => (0.45, 0.78, 1.0),
        LogKind::System(PlayerMessageKind::Warn) => (1.0, 0.52, 0.16),
        LogKind::System(PlayerMessageKind::Error) => (1.0, 0.25, 0.22),
    };
    Color::srgba(red, green, blue, alpha)
}

fn capture_chat(
    mut typed: MessageReader<KeyboardInput>,
    mut mode: ResMut<InputMode>,
    mut draft: ResMut<ChatLine>,
    mut history: ResMut<ChatHistory>,
    mut outbound: Option<ResMut<Outbound>>,
) {
    if *mode != InputMode::Chat || mode.is_changed() {
        // Always drain: the T that opened chat and keys typed elsewhere must never leak
        // into the draft on a later frame.
        typed.clear();
        return;
    }

    for key in typed.read() {
        if key.state == ButtonState::Pressed && key.logical_key == Key::ArrowUp {
            if let Some(last) = &history.0 {
                draft.0.clone_from(last);
            }
            continue;
        }

        // The reading of a key is `ui/text_input.rs`'s, shared with the map's note field.
        // What stays here is what makes this line chat's: the mode it lives in, and that
        // `Enter` is a message to the world rather than a mark on a map.
        match apply_key(key, &mut draft.0, DRAFT_LIMIT_BYTES) {
            Some(TextEdit::Cancelled) => {
                draft.0.clear();
                set_mode(&mut mode, InputMode::Playing);
                return;
            }
            Some(TextEdit::Submitted) => {
                let line = std::mem::take(&mut draft.0);
                if !line.trim().is_empty() {
                    history.0 = Some(line.clone());
                }
                send_line(line, outbound.as_deref_mut());
                set_mode(&mut mode, InputMode::Playing);
                return;
            }
            Some(TextEdit::Typed) | None => {}
        }
    }
}

fn send_line(line: String, outbound: Option<&mut Outbound>) {
    let Some(frame) = outgoing_frame(&line) else {
        return;
    };
    let Some(outbound) = outbound else {
        return;
    };
    if outbound.send(frame) == Sent::Dropped {
        warn!("the outbound queue was full; one chat or party request was dropped");
    }
}

fn outgoing_frame(line: &str) -> Option<Vec<u8>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !line.starts_with('/') {
        return Some(encode_chat_request(&ChatRequest {
            text: trimmed.to_owned(),
        }));
    }

    let party = match trimmed {
        "/accept" => Some((PartyAction::Accept, "")),
        "/decline" => Some((PartyAction::Decline, "")),
        "/leave" => Some((PartyAction::Leave, "")),
        _ => trimmed
            .strip_prefix("/invite ")
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .map(|target| (PartyAction::Invite, target))
            .or_else(|| {
                line.strip_prefix("/kick ")
                    .map(str::trim)
                    .filter(|target| !target.is_empty())
                    .map(|target| (PartyAction::Kick, target))
            }),
    };
    match party {
        Some((action, target_name)) => Some(encode_party_request(&PartyRequest {
            action,
            target_name: target_name.to_owned(),
        })),
        // Preserve the raw command byte-for-byte. Parsing and every outcome belong
        // to the server; local trimming here would be a second parser whose answer
        // could disagree with it.
        None => Some(encode_chat_request(&ChatRequest {
            text: line.to_owned(),
        })),
    }
}

/// The mark a shortened value ends with.
///
/// Three full stops rather than `…` (U+2026), and the reason is the font rather than
/// taste: Bevy's `default_font` is a 95-glyph ASCII subset of FiraMono, so an ellipsis
/// draws as nothing at all and a name that *had* been shortened would read as a name that
/// simply ended there - the one thing a truncation mark exists to deny.
///
/// It costs no width. [`bounded_display`] spends the mark's three characters out of
/// `limit` rather than adding them to it, so what reaches Bevy's layout engine is still at
/// most `limit` characters however hostile the value was — and, when `limit` is too small
/// to hold the whole mark, the mark is the part that gives way rather than the bound.
const TRUNCATION_MARK: &str = "...";

/// What a character Bevy's layout engine must not see is shown as.
///
/// It was U+FFFD, the replacement character, which is the conventional answer and is not
/// in this font either - so a control character was replaced by a glyph of zero advance
/// and vanished as completely as it would have with nothing replacing it. A question mark
/// occupies its column, which is the whole of what this substitution is for.
const CONTROL_MARK: char = '?';

/// One display character, with anything the layout engine must not see replaced.
///
/// Only controls are touched. A player's name may legitimately be in a script this font
/// cannot draw, and what to do about that is a question about names rather than about the
/// strings this client composes.
fn displayable(character: char) -> char {
    if character.is_control() {
        CONTROL_MARK
    } else {
        character
    }
}

/// `value`, cut to at most `limit` characters and safe for Bevy's layout engine.
///
/// The bound is unconditional. Every caller here passes a limit far larger than
/// [`TRUNCATION_MARK`], but the mark is still only ever *taken from* `limit` — with a limit
/// of two the output is `..`, with one it is `.`, with zero it is empty — because a helper
/// whose contract holds only for the arguments it happens to be given today is a bound
/// nobody can rely on tomorrow.
fn bounded_display(value: &str, limit: usize) -> String {
    // One character past the bound is what makes this a truncation rather than a fit: a
    // value of exactly `limit` characters is shown whole, as it always was.
    let head: Vec<char> = value.chars().take(limit.saturating_add(1)).collect();
    let mut shown = String::with_capacity(limit.saturating_mul(4));
    if head.len() <= limit {
        shown.extend(head.into_iter().map(displayable));
        return shown;
    }
    let kept = limit.saturating_sub(TRUNCATION_MARK.chars().count());
    shown.extend(head.into_iter().take(kept).map(displayable));
    shown.extend(TRUNCATION_MARK.chars().take(limit));
    shown
}

fn render_chat(
    mode: Res<InputMode>,
    draft: Res<ChatLine>,
    log: Res<ChatLog>,
    time: Res<Time<Real>>,
    mut lines: Query<(&ChatText, &mut Text, &mut TextColor)>,
    mut input: Query<&mut Text, (With<ChatInput>, Without<ChatText>)>,
) {
    let visible = matches!(*mode, InputMode::Playing | InputMode::Chat);
    let now = time.elapsed();
    for (slot, mut text, mut colour) in &mut lines {
        let Some(line) = log.0.get(slot.0) else {
            text.0.clear();
            colour.0 = Color::NONE;
            continue;
        };
        text.0.clone_from(&line.text);
        let alpha = line_alpha(*mode, visible, now.saturating_sub(line.added));
        colour.0 = message_colour(line.kind, alpha);
    }

    let Ok(mut input) = input.single_mut() else {
        return;
    };
    if *mode == InputMode::Chat {
        input.0 = format!("> {}", draft.0);
    } else {
        input.0.clear();
    }
}

fn line_alpha(mode: InputMode, visible: bool, age: Duration) -> f32 {
    if !visible {
        0.0
    } else if mode == InputMode::Chat {
        1.0
    } else {
        (1.0 - age.as_secs_f32() / LINE_LIFETIME.as_secs_f32()).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, ChatMessage, PartyInvite, SessionParams};

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

    fn capture_app(outbound: Option<Outbound>) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<KeyboardInput>()
            .insert_resource(InputMode::Chat)
            .init_resource::<ChatLine>()
            .init_resource::<ChatHistory>()
            .init_resource::<ChatLog>()
            .add_systems(Update, capture_chat);
        if let Some(outbound) = outbound {
            app.insert_resource(outbound);
        }
        // Settle the inserted mode's change flag; that first frame belongs to the key
        // that opened chat and is deliberately drained.
        app.update();
        app
    }

    fn type_key(app: &mut App, logical_key: Key) {
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::KeyA,
            logical_key,
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
    }

    /// The draft is still bounded, now through the shared field.
    ///
    /// What the bound *is* -- bytes, whole characters, no controls -- is
    /// `ui/text_input.rs`'s own test. This is the wiring: that this line passes its own
    /// limit down, and that a full line stops taking characters.
    #[test]
    fn the_draft_is_still_bounded_through_the_shared_field() {
        let mut app = capture_app(None);
        for _ in 0..DRAFT_LIMIT_BYTES + 8 {
            type_key(&mut app, Key::Character("a".into()));
        }
        app.update();
        assert_eq!(
            app.world().resource::<ChatLine>().0.len(),
            DRAFT_LIMIT_BYTES
        );
    }

    #[test]
    fn party_commands_are_typed_and_other_slash_lines_reach_chat_verbatim() {
        let cases = [
            ("/invite Eivor", PartyAction::Invite, "Eivor"),
            ("/accept ", PartyAction::Accept, ""),
            ("/decline ", PartyAction::Decline, ""),
            ("/leave   ", PartyAction::Leave, ""),
            ("/kick Eivor ", PartyAction::Kick, "Eivor"),
        ];
        for (line, action, target_name) in cases {
            assert_eq!(
                outgoing_frame(line),
                Some(encode_party_request(&PartyRequest {
                    action,
                    target_name: target_name.to_owned(),
                }))
            );
        }
        assert_eq!(
            outgoing_frame("hello"),
            Some(encode_chat_request(&ChatRequest {
                text: "hello".to_owned()
            }))
        );
        assert_eq!(outgoing_frame("   "), None);
        assert_eq!(
            outgoing_frame("/teleport 1 2 3  "),
            Some(encode_chat_request(&ChatRequest {
                text: "/teleport 1 2 3  ".to_owned()
            }))
        );
        assert_eq!(
            outgoing_frame("/dance"),
            Some(encode_chat_request(&ChatRequest {
                text: "/dance".to_owned()
            }))
        );
        assert_eq!(
            outgoing_frame("/invite   "),
            Some(encode_chat_request(&ChatRequest {
                text: "/invite   ".to_owned()
            }))
        );
    }

    #[test]
    fn ring_keeps_the_last_eight_in_order() {
        let mut log = ChatLog::default();
        for number in 0..10 {
            log.push(number.to_string(), Duration::from_secs(number));
        }
        assert_eq!(log.0.len(), LINE_COUNT);
        assert_eq!(log.0.front().unwrap().text, "2");
        assert_eq!(log.0.back().unwrap().text, "9");
    }

    #[test]
    fn hostile_display_text_is_bounded_and_single_line() {
        assert_eq!(bounded_display("Ei\nvor", 8), "Ei?vor");
        // Exactly the bound is shown whole; one character more spends three of them on
        // the mark, so the bound itself never moves.
        assert_eq!(bounded_display("abcde", 5), "abcde");
        assert_eq!(bounded_display("abcdefghij", 5), "ab...");
        assert_eq!(bounded_display("abcdefghij", 5).chars().count(), 5);
        // A limit too small to hold the mark cuts the mark, never the bound: the promise
        // is that the layout engine sees at most `limit` characters, for every limit.
        for limit in 0..=6 {
            assert!(
                bounded_display("abcdefghij", limit).chars().count() <= limit,
                "a limit of {limit} produced more than {limit} characters"
            );
        }
        assert_eq!(bounded_display("ab", 1), ".");
        assert_eq!(bounded_display("ab", 0), "");
    }

    #[test]
    fn inboxes_keep_every_value_in_wire_order() {
        let mut chat = ChatInbox::default();
        chat.push(ChatEntry::PartyInvite(PartyInvite {
            from_entity_id: 11,
            from_name: "Eivor".to_owned(),
            expires_ms: 5_000,
        }));
        for sender_entity_id in [7, 9] {
            chat.push(ChatEntry::Message(ChatMessage {
                sender_entity_id,
                sender_name: sender_entity_id.to_string(),
                text: "hello".to_owned(),
            }));
        }
        assert_eq!(chat.pending(), 3);
        let entries = chat.take();
        assert!(matches!(
            &entries[0],
            ChatEntry::PartyInvite(invite) if invite.from_entity_id == 11
        ));
        assert!(matches!(
            &entries[1],
            ChatEntry::Message(message) if message.sender_entity_id == 7
        ));
        assert!(matches!(
            &entries[2],
            ChatEntry::Message(message) if message.sender_entity_id == 9
        ));
    }

    fn server_ingest_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ChatInbox>()
            .init_resource::<ChatLog>()
            .add_message::<PlayerMessage>()
            .add_systems(
                Update,
                (ingest_server_lines, ingest_player_messages).chain(),
            );
        app
    }

    #[test]
    fn an_invite_remains_a_highlighted_line_with_both_commands() {
        let mut app = server_ingest_app();
        app.world_mut()
            .resource_mut::<ChatInbox>()
            .push(ChatEntry::PartyInvite(PartyInvite {
                from_entity_id: 11,
                from_name: "Eivor".to_owned(),
                expires_ms: 5_000,
            }));
        app.update();
        let line = app.world().resource::<ChatLog>().0.back().unwrap();
        assert_eq!(
            line.text,
            "Eivor invites you to a party - /accept or /decline"
        );
        assert_eq!(line.kind, LogKind::Highlight);
    }

    #[test]
    fn the_reserved_command_answer_is_a_server_line() {
        let mut app = server_ingest_app();
        app.world_mut()
            .resource_mut::<ChatInbox>()
            .push(ChatEntry::Message(ChatMessage {
                sender_entity_id: 7,
                sender_name: COMMAND_SENDER_NAME.to_owned(),
                text: "Development commands are disabled.".to_owned(),
            }));
        app.update();
        let line = app.world().resource::<ChatLog>().0.back().unwrap();
        assert_eq!(line.text, "[SERVER] Development commands are disabled.");
        assert_eq!(line.kind, LogKind::System(PlayerMessageKind::Server));
    }

    #[test]
    fn command_and_player_lines_keep_their_wire_order() {
        let mut app = server_ingest_app();
        for (sender_name, text) in [(COMMAND_SENDER_NAME, "done"), ("Eivor", "hello")] {
            app.world_mut()
                .resource_mut::<ChatInbox>()
                .push(ChatEntry::Message(ChatMessage {
                    sender_entity_id: 7,
                    sender_name: sender_name.to_owned(),
                    text: text.to_owned(),
                }));
        }
        app.update();
        let lines: Vec<&str> = app
            .world()
            .resource::<ChatLog>()
            .0
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(lines, ["[SERVER] done", "Eivor: hello"]);
    }

    #[test]
    fn ordinary_player_chat_keeps_name_and_has_no_system_tag() {
        let mut app = server_ingest_app();
        app.world_mut()
            .resource_mut::<ChatInbox>()
            .push(ChatEntry::Message(ChatMessage {
                sender_entity_id: 7,
                sender_name: "Eivor".to_owned(),
                text: "hello".to_owned(),
            }));
        app.update();
        let line = app.world().resource::<ChatLog>().0.back().unwrap();
        assert_eq!(line.text, "Eivor: hello");
        assert_eq!(line.kind, LogKind::Player);
    }

    #[test]
    fn every_message_kind_has_one_tag_and_colour() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ChatLog>()
            .add_message::<PlayerMessage>()
            .add_systems(Update, ingest_player_messages);
        for kind in [
            PlayerMessageKind::Server,
            PlayerMessageKind::Info,
            PlayerMessageKind::Warn,
            PlayerMessageKind::Error,
        ] {
            app.world_mut()
                .write_message(PlayerMessage::new(kind, "message"));
        }
        app.update();

        let log = app.world().resource::<ChatLog>();
        let expected = [
            ("[SERVER] message", PlayerMessageKind::Server),
            ("[INFO] message", PlayerMessageKind::Info),
            ("[WARN] message", PlayerMessageKind::Warn),
            ("[ERROR] message", PlayerMessageKind::Error),
        ];
        for (line, (text, kind)) in log.0.iter().zip(expected) {
            assert_eq!(line.text, text);
            assert_eq!(line.kind, LogKind::System(kind));
        }
        assert_eq!(
            message_colour(LogKind::System(PlayerMessageKind::Server), 0.5),
            Color::srgba(1.0, 0.72, 0.25, 0.5)
        );
        assert_eq!(
            message_colour(LogKind::System(PlayerMessageKind::Info), 0.5),
            Color::srgba(0.45, 0.78, 1.0, 0.5)
        );
        assert_eq!(
            message_colour(LogKind::System(PlayerMessageKind::Warn), 0.5),
            Color::srgba(1.0, 0.52, 0.16, 0.5)
        );
        assert_eq!(
            message_colour(LogKind::System(PlayerMessageKind::Error), 0.5),
            Color::srgba(1.0, 0.25, 0.22, 0.5)
        );
    }

    #[test]
    fn party_lines_are_dropped_once_the_session_is_gone() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<PartyLogInbox>()
            .init_resource::<ChatLog>()
            .add_message::<PlayerMessage>()
            .add_systems(Update, (ingest_party_lines, ingest_player_messages).chain());
        app.world_mut()
            .resource_mut::<PartyLogInbox>()
            .push("Eivor joined the party".to_owned());
        app.update();
        assert!(app.world().resource::<ChatLog>().0.is_empty());
        assert!(
            app.world_mut()
                .resource_mut::<PartyLogInbox>()
                .take()
                .is_empty()
        );

        app.insert_resource(session());
        app.world_mut()
            .resource_mut::<PartyLogInbox>()
            .push("Eivor joined the party".to_owned());
        app.update();
        assert_eq!(
            app.world().resource::<ChatLog>().0.back().unwrap().text,
            "Eivor joined the party"
        );
    }

    #[test]
    fn enter_sends_and_escape_discards_before_returning_to_play() {
        let (outbound, receiver) = Outbound::to_a_test(2);
        let mut app = capture_app(Some(outbound));
        type_key(&mut app, Key::Character("hello".into()));
        type_key(&mut app, Key::Enter);
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        assert_eq!(
            receiver.try_recv().unwrap(),
            encode_chat_request(&ChatRequest {
                text: "hello".to_owned()
            })
        );

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        type_key(&mut app, Key::Character("discard me".into()));
        type_key(&mut app, Key::Escape);
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        assert_eq!(app.world().resource::<ChatLine>().0, "");
        assert!(receiver.try_recv().is_err());
    }

    fn reopen_chat(app: &mut App) {
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0, "");
    }

    #[test]
    fn arrow_up_recalls_one_message_repeatedly_and_the_draft_stays_editable() {
        let mut app = capture_app(None);
        type_key(&mut app, Key::Character("hello".into()));
        type_key(&mut app, Key::Enter);
        app.update();

        reopen_chat(&mut app);
        type_key(&mut app, Key::ArrowUp);
        type_key(&mut app, Key::ArrowUp);
        type_key(&mut app, Key::Backspace);
        type_key(&mut app, Key::Character("!".into()));
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0, "hell!");

        for _ in 0..5 {
            type_key(&mut app, Key::Backspace);
        }
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0, "");
    }

    #[test]
    fn arrow_up_recalls_a_slash_command_byte_for_byte() {
        let mut app = capture_app(None);
        type_key(&mut app, Key::Character("/teleport 1 2 3  ".into()));
        type_key(&mut app, Key::Enter);
        app.update();

        reopen_chat(&mut app);
        type_key(&mut app, Key::ArrowUp);
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0, "/teleport 1 2 3  ");
    }

    #[test]
    fn empty_submission_and_escape_do_not_replace_recall() {
        let mut app = capture_app(None);
        type_key(&mut app, Key::Character("remember me".into()));
        type_key(&mut app, Key::Enter);
        app.update();

        reopen_chat(&mut app);
        type_key(&mut app, Key::Character("   ".into()));
        type_key(&mut app, Key::Enter);
        app.update();

        reopen_chat(&mut app);
        type_key(&mut app, Key::Character("discard me".into()));
        type_key(&mut app, Key::Escape);
        app.update();

        reopen_chat(&mut app);
        type_key(&mut app, Key::ArrowUp);
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0, "remember me");
    }

    #[test]
    fn opening_frame_is_drained_and_fade_uses_real_elapsed_time() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<KeyboardInput>()
            .insert_resource(InputMode::Chat)
            .init_resource::<ChatLine>()
            .init_resource::<ChatHistory>()
            .init_resource::<ChatLog>()
            .add_systems(Update, capture_chat);
        type_key(&mut app, Key::Character("t".into()));
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0, "");

        assert!(line_alpha(InputMode::Playing, true, Duration::ZERO) > 0.99);
        assert!(line_alpha(InputMode::Playing, true, Duration::from_millis(11_900)) > 0.0);
        assert_eq!(line_alpha(InputMode::Playing, true, LINE_LIFETIME), 0.0);
        assert_eq!(
            line_alpha(InputMode::Chat, true, Duration::from_secs(60)),
            1.0
        );
    }
}
