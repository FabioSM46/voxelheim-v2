//! The transient world-chat log and the one-line text entry surface.
//!
//! Received text is presentation only: this module bounds it for layout and never parses
//! it as a command or trusts its sender name as identity. Commands exist only on the
//! locally typed line and become typed party requests for the authoritative server.

use std::collections::VecDeque;
use std::time::Duration;

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::time::Real;

use crate::net::{
    ChatInbox, ChatRequest, DrainNetwork, Outbound, PartyAction, PartyInviteInbox, PartyRequest,
    Sent, encode_chat_request, encode_party_request,
};
use crate::player::{ApplyInputMode, InputMode};

use super::set_mode;

const LINE_COUNT: usize = 8;
const LINE_LIFETIME: Duration = Duration::from_secs(12);
const DRAFT_LIMIT_BYTES: usize = 256;
const SENDER_CHARACTERS: usize = 48;
const MESSAGE_CHARACTERS: usize = 256;
const FONT_SIZE: FontSize = FontSize::Px(17.0);
const LEFT: f32 = 16.0;
const INPUT_BOTTOM: f32 = 44.0;
const LOG_BOTTOM: f32 = 70.0;

#[derive(Resource, Debug, Default, PartialEq, Eq)]
struct ChatLine(String);

#[derive(Debug, Clone, PartialEq)]
struct LogLine {
    text: String,
    added: Duration,
}

#[derive(Resource, Debug, Default)]
struct ChatLog(VecDeque<LogLine>);

impl ChatLog {
    fn push(&mut self, text: String, now: Duration) {
        if self.0.len() == LINE_COUNT {
            self.0.pop_front();
        }
        self.0.push_back(LogLine { text, added: now });
    }
}

#[derive(Component)]
struct ChatText(usize);

#[derive(Component)]
struct ChatInput;

pub(super) struct ChatUiPlugin;

impl Plugin for ChatUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChatLine>()
            .init_resource::<ChatLog>()
            .init_resource::<ChatInbox>()
            .init_resource::<PartyInviteInbox>()
            .add_message::<KeyboardInput>()
            .add_systems(Startup, spawn_chat)
            .add_systems(
                Update,
                (
                    ingest_server_lines.after(DrainNetwork),
                    capture_chat.after(ApplyInputMode),
                    render_chat,
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
    mut invites: ResMut<PartyInviteInbox>,
    mut log: ResMut<ChatLog>,
) {
    let now = time.elapsed();
    for message in chat.take() {
        let sender = bounded_display(&message.sender_name, SENDER_CHARACTERS);
        let text = bounded_display(&message.text, MESSAGE_CHARACTERS);
        log.push(format!("{sender}: {text}"), now);
    }
    for invite in invites.take() {
        let sender = bounded_display(&invite.from_name, SENDER_CHARACTERS);
        log.push(
            format!("{sender} invites you to a party — /accept or /decline"),
            now,
        );
    }
}

fn capture_chat(
    mut typed: MessageReader<KeyboardInput>,
    mut mode: ResMut<InputMode>,
    mut draft: ResMut<ChatLine>,
    mut log: ResMut<ChatLog>,
    time: Res<Time<Real>>,
    mut outbound: Option<ResMut<Outbound>>,
) {
    if *mode != InputMode::Chat || mode.is_changed() {
        // Always drain: the T that opened chat and keys typed elsewhere must never leak
        // into the draft on a later frame.
        typed.clear();
        return;
    }

    for key in typed.read() {
        if key.state != ButtonState::Pressed {
            continue;
        }
        match &key.logical_key {
            Key::Escape => {
                draft.0.clear();
                set_mode(&mut mode, InputMode::Playing);
                return;
            }
            Key::Enter => {
                let line = std::mem::take(&mut draft.0);
                send_line(line, outbound.as_deref_mut(), &mut log, time.elapsed());
                set_mode(&mut mode, InputMode::Playing);
                return;
            }
            Key::Backspace => {
                draft.0.pop();
            }
            Key::Space => push_character(&mut draft.0, ' '),
            Key::Character(text) => {
                for character in text.chars() {
                    push_character(&mut draft.0, character);
                }
            }
            _ => {}
        }
    }
}

fn push_character(line: &mut String, character: char) {
    if character.is_control() || line.len() + character.len_utf8() > DRAFT_LIMIT_BYTES {
        return;
    }
    line.push(character);
}

fn send_line(line: String, outbound: Option<&mut Outbound>, log: &mut ChatLog, now: Duration) {
    let Some(frame) = outgoing_frame(&line, log, now) else {
        return;
    };
    let Some(outbound) = outbound else {
        return;
    };
    if outbound.send(frame) == Sent::Dropped {
        warn!("the outbound queue was full; one chat or party request was dropped");
    }
}

fn outgoing_frame(line: &str, log: &mut ChatLog, now: Duration) -> Option<Vec<u8>> {
    if line.trim().is_empty() {
        return None;
    }
    if !line.starts_with('/') {
        return Some(encode_chat_request(&ChatRequest {
            text: line.to_owned(),
        }));
    }

    let party = match line {
        "/accept" => Some((PartyAction::Accept, "")),
        "/decline" => Some((PartyAction::Decline, "")),
        "/leave" => Some((PartyAction::Leave, "")),
        _ => line
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
        None => {
            log.push(
                format!("Unknown command: {}", bounded_display(line, 64)),
                now,
            );
            None
        }
    }
}

fn bounded_display(value: &str, limit: usize) -> String {
    let mut characters = value.chars();
    let mut shown = String::with_capacity(limit.saturating_mul(4));
    for position in 0..limit {
        let Some(character) = characters.next() else {
            return shown;
        };
        if position + 1 == limit && characters.next().is_some() {
            shown.push('…');
            return shown;
        }
        shown.push(if character.is_control() {
            '\u{fffd}'
        } else {
            character
        });
    }
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
        colour.0 = Color::srgba(1.0, 1.0, 1.0, alpha);
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
    use crate::net::{ChatMessage, PartyInvite};

    fn capture_app(outbound: Option<Outbound>) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<KeyboardInput>()
            .insert_resource(InputMode::Chat)
            .init_resource::<ChatLine>()
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

    #[test]
    fn draft_limit_is_utf8_safe_and_rejects_controls() {
        let mut line = "a".repeat(DRAFT_LIMIT_BYTES - 1);
        push_character(&mut line, 'é');
        assert_eq!(line.len(), DRAFT_LIMIT_BYTES - 1);
        push_character(&mut line, 'b');
        push_character(&mut line, '\n');
        assert_eq!(line.len(), DRAFT_LIMIT_BYTES);
        assert!(line.is_char_boundary(line.len()));
        line.pop();
        assert_eq!(line.len(), DRAFT_LIMIT_BYTES - 1);
    }

    #[test]
    fn commands_encode_only_the_five_typed_party_requests() {
        let now = Duration::ZERO;
        let mut log = ChatLog::default();
        let cases = [
            ("/invite Eivor", PartyAction::Invite, "Eivor"),
            ("/accept", PartyAction::Accept, ""),
            ("/decline", PartyAction::Decline, ""),
            ("/leave", PartyAction::Leave, ""),
            ("/kick Eivor", PartyAction::Kick, "Eivor"),
        ];
        for (line, action, target_name) in cases {
            assert_eq!(
                outgoing_frame(line, &mut log, now),
                Some(encode_party_request(&PartyRequest {
                    action,
                    target_name: target_name.to_owned(),
                }))
            );
        }
        assert_eq!(
            outgoing_frame("hello", &mut log, now),
            Some(encode_chat_request(&ChatRequest {
                text: "hello".to_owned()
            }))
        );
        assert_eq!(outgoing_frame("   ", &mut log, now), None);
        assert_eq!(outgoing_frame("/dance", &mut log, now), None);
        assert_eq!(log.0.back().unwrap().text, "Unknown command: /dance");
        assert_eq!(outgoing_frame("/invite   ", &mut log, now), None);
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
        assert_eq!(bounded_display("Ei\nvor", 8), "Ei�vor");
        assert_eq!(bounded_display("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn inboxes_keep_every_value_in_wire_order() {
        let mut chat = ChatInbox::default();
        for sender_entity_id in [7, 9] {
            chat.push(ChatMessage {
                sender_entity_id,
                sender_name: sender_entity_id.to_string(),
                text: "hello".to_owned(),
            });
        }
        assert_eq!(chat.pending(), 2);
        assert_eq!(
            chat.take()
                .into_iter()
                .map(|message| message.sender_entity_id)
                .collect::<Vec<_>>(),
            vec![7, 9]
        );

        let mut invites = PartyInviteInbox::default();
        invites.push(PartyInvite {
            from_entity_id: 11,
            from_name: "Eivor".to_owned(),
            expires_ms: 5_000,
        });
        assert_eq!(invites.take()[0].from_entity_id, 11);
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

    #[test]
    fn opening_frame_is_drained_and_fade_uses_real_elapsed_time() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<KeyboardInput>()
            .insert_resource(InputMode::Chat)
            .init_resource::<ChatLine>()
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
