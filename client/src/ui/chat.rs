//! The transient world-chat log and the one-line text entry surface.
//!
//! Received text is presentation only: this module bounds it for layout and never parses
//! it as a command or trusts its sender name as identity. The five party commands become
//! typed requests; every other slash-prefixed line reaches the authoritative server as
//! chat-carried command input.

use std::collections::VecDeque;
use std::ops::Range;
use std::time::Duration;

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::text::TextLayoutInfo;
use bevy::time::Real;
use bevy::ui::{ComputedNode, UiGlobalTransform, UiSystems};
use bevy::window::PrimaryWindow;

use crate::net::{
    ChatEntry, ChatInbox, ChatRequest, DrainNetwork, Outbound, PartyAction, PartyRequest, Sent,
    Session, encode_chat_request, encode_party_request,
};
use crate::player::{ApplyInputMode, ApplySnapshots, InputMode, PartyLogInbox};

use super::chat_selection::{DrawnLine, LogSelection, glyph_cells, point_at};
use super::text_input::{
    FieldInput, FieldLook, FieldPieces, FieldSpan, TextEdit, TextField, paint_span,
    spawn_field_spans,
};
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

/// The draft: the line, its cursor and its selection, edited by `ui/text_input.rs`.
#[derive(Resource, Debug, Default, PartialEq, Eq)]
struct ChatLine(TextField);

/// The one submitted line this process can recall.
///
/// This stays beside the draft rather than in the shared text-input helper: remembering a
/// submission is chat behaviour, and the map's note field must keep treating `ArrowUp` as a
/// no-op key. It is deliberately a single optional line rather than a growing session log.
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
    /// How many lines the log had taken before this one: what a selection names it by, so a
    /// new line pushing the ring along cannot move a selection onto its neighbour.
    serial: u64,
}

/// The last [`LINE_COUNT`] lines, oldest first, and how many lines the log has ever taken.
#[derive(Resource, Debug, Default)]
pub(super) struct ChatLog(VecDeque<LogLine>, u64);

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
            serial: self.1,
        });
        self.1 = self.1.wrapping_add(1);
    }

    /// Every line with its serial, oldest first.
    fn lines(&self) -> impl Iterator<Item = (u64, &str)> {
        self.0.iter().map(|line| (line.serial, line.text.as_str()))
    }

    /// The serial of the oldest line still held, when any is.
    fn oldest(&self) -> Option<u64> {
        self.0.front().map(|line| line.serial)
    }

    fn push_highlighted(&mut self, text: String, now: Duration) {
        self.push_kind(text, now, LogKind::Highlight);
    }
}

/// One row of the log, by its place in the ring: row 0 draws the oldest line held.
#[derive(Component)]
struct ChatText(usize);

/// Marks the spans a log row is drawn in, by the row they belong to.
///
/// A row is drawn in the draft's four spans rather than as one `Text`, and for the draft's reason:
/// a highlighted stretch of a line is a span with a background, and splitting the line in
/// [`log_pieces`] keeps each glyph where it was.
#[derive(Component, Clone)]
struct LogSpan(usize);

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
            .init_resource::<LogSelection>()
            .init_resource::<ChatInbox>()
            .init_resource::<PartyLogInbox>()
            .add_message::<KeyboardInput>()
            .add_message::<PlayerMessage>()
            .add_systems(Startup, spawn_chat)
            // After `text_system`, which writes each row's glyphs in `PostLayout`: a press is hit
            // against the layout the player was looking at, not the one before it.
            .add_systems(PostUpdate, select_in_log.after(UiSystems::PostLayout))
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
                let font = TextFont {
                    font_size: FONT_SIZE,
                    ..default()
                };
                root.spawn((
                    ChatText(index),
                    Text::new(String::new()),
                    font.clone(),
                    TextColor(Color::NONE),
                    TextShadow::default(),
                ))
                .with_children(|row| {
                    spawn_field_spans(
                        row,
                        &log_pieces("", None),
                        &font,
                        Color::NONE,
                        LogSpan(index),
                    );
                });
            }
        });

    commands
        .spawn((
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
        ))
        .with_children(|input| {
            spawn_field_spans(
                input,
                &TextField::default().pieces(false),
                &TextFont {
                    font_size: FONT_SIZE,
                    ..default()
                },
                Color::WHITE,
                DraftSpan,
            );
        });
}

/// Marks the draft's spans, which `ui/text_input.rs` fills.
#[derive(Component, Clone)]
struct DraftSpan;

/// The draft's spans, apart from the log rows' spans that share their components.
type DraftSpans<'w, 's> = Query<
    'w,
    's,
    (
        &'static FieldSpan,
        &'static mut TextSpan,
        &'static mut TextColor,
        &'static mut TextBackgroundColor,
    ),
    (With<DraftSpan>, Without<LogSpan>),
>;

/// The log rows' spans, apart from the draft's.
type LogSpans<'w, 's> = Query<
    'w,
    's,
    (
        &'static LogSpan,
        &'static FieldSpan,
        &'static mut TextSpan,
        &'static mut TextColor,
        &'static mut TextBackgroundColor,
    ),
    Without<DraftSpan>,
>;

/// A log line cut into the four spans a row is drawn in: before the selection, the selection, an
/// unused piece where the draft keeps its caret, and after.
fn log_pieces(text: &str, selected: Option<Range<usize>>) -> FieldPieces {
    let selected = selected.unwrap_or(text.len()..text.len());
    let look = if selected.is_empty() {
        FieldLook::Plain
    } else {
        FieldLook::Selected
    };
    [
        (text[..selected.start].to_owned(), FieldLook::Plain),
        (text[selected.clone()].to_owned(), look),
        (String::new(), FieldLook::Plain),
        (text[selected.end..].to_owned(), FieldLook::Plain),
    ]
}

/// Reads a press, a drag and a release of the primary button over the log into [`LogSelection`].
///
/// **Only while chat is open.** In any other mode the pointer is captured or belongs to a panel,
/// so the selection is dropped and nothing is read: closing chat clears it, and nothing in the log
/// can be selected while it is closed. A press on a row with a line in it begins a selection there;
/// a press anywhere else clears it. A drag that has begun follows the pointer to the nearest row
/// even outside the log, and — the inventory's rule — losing the pointer ends the drag, because a
/// release outside the window is not delivered on every platform.
///
/// **The keyboard is not read here**, and that is what keeps typing on the draft: the selection is
/// something `Control+C` consults in [`capture_chat`], never a second place keys can go.
fn select_in_log(
    mode: Res<InputMode>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    log: Res<ChatLog>,
    rows: Query<(
        &ChatText,
        &ComputedNode,
        &UiGlobalTransform,
        &TextLayoutInfo,
    )>,
    mut selection: ResMut<LogSelection>,
) {
    if *mode != InputMode::Chat {
        if *selection != LogSelection::default() {
            selection.clear();
        }
        return;
    }
    selection.forget_lines_before(log.oldest());

    let Some(buttons) = buttons.filter(|buttons| buttons.pressed(MouseButton::Left)) else {
        if selection.is_dragging() {
            selection.release();
        }
        return;
    };
    let pressed = buttons.just_pressed(MouseButton::Left);
    if !pressed && !selection.is_dragging() {
        return;
    }
    let Some(cursor) = windows.iter().next().and_then(Window::cursor_position) else {
        if pressed {
            selection.clear();
        } else {
            selection.release();
        }
        return;
    };

    // Physical pixels throughout. `ComputedNode` documents its sizes as physical, the transform
    // is built from the same layout, and `bevy_ui`'s own picking backend hit-tests
    // `TextLayoutInfo::run_geometry` with the pointer scaled to physical pixels; the logical
    // cursor is taken there by a node's inverse scale factor. Every row is in the one window, so
    // any row's factor is every row's: it is read once rather than from whichever row is last.
    let scale = rows
        .iter()
        .next()
        .map_or(1.0, |(_, node, _, _)| node.inverse_scale_factor);
    let drawn: Vec<DrawnLine<'_>> = rows
        .iter()
        .filter_map(|(row, node, transform, layout)| {
            let line = log.0.get(row.0)?;
            let content = node.content_box();
            Some(DrawnLine {
                line: line.serial,
                text: &line.text,
                frame: Rect::from_corners(
                    transform.translation + content.min,
                    transform.translation + content.max,
                ),
                cells: glyph_cells(layout),
            })
        })
        .collect();
    let pointer = cursor / scale;

    match point_at(&drawn, pointer, pressed) {
        Some(point) if pressed => selection.begin(point),
        Some(point) => selection.extend(point),
        None if pressed => selection.clear(),
        None => {}
    }
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

#[allow(clippy::too_many_arguments)] // The field's keys and clipboard, and the log's selection.
fn capture_chat(
    time: Res<Time<Real>>,
    mut typed: MessageReader<KeyboardInput>,
    mut field: FieldInput,
    mut mode: ResMut<InputMode>,
    mut draft: ResMut<ChatLine>,
    mut history: ResMut<ChatHistory>,
    mut outbound: Option<ResMut<Outbound>>,
    mut log: ResMut<ChatLog>,
    selection: Res<LogSelection>,
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
                draft.0.set_text(last);
            }
            continue;
        }

        // `Control+C` copies the log's selection when there is one, and is the draft's own
        // shortcut otherwise. Only `C`: the log is read-only, so a cut or a paste always means
        // the draft, and no other key is ever taken from it.
        if key.state == ButtonState::Pressed
            && field.modifiers().control
            && matches!(&key.logical_key, Key::Character(letter) if letter.eq_ignore_ascii_case("c"))
            && let Some(copied) = selection.text(log.lines())
        {
            field.copy(&copied);
            continue;
        }

        // The reading of a key is `ui/text_input.rs`'s, shared with the map's note field.
        // What stays here is what makes this line chat's: the mode it lives in, and that
        // `Enter` is a message to the world rather than a mark on a map.
        match field.apply(&mut draft.0, key, DRAFT_LIMIT_BYTES) {
            Some(TextEdit::Cancelled) => {
                draft.0.clear();
                set_mode(&mut mode, InputMode::Playing);
                return;
            }
            Some(TextEdit::Submitted) => {
                let line = draft.0.take();
                if !line.trim().is_empty() {
                    history.0 = Some(line.clone());
                }
                send_line(line, outbound.as_deref_mut(), &mut log, time.elapsed());
                set_mode(&mut mode, InputMode::Playing);
                return;
            }
            Some(TextEdit::Typed) | None => {}
        }
    }
}

/// Sends one chat or party frame, and reports directly to the log when it does not leave.
///
/// **Written straight to [`ChatLog`] rather than through the [`PlayerMessage`] bus.** This
/// module already owns the log it would end up in, and going by way of a `MessageWriter`
/// here would ask the reader — [`ingest_player_messages`], later in the same chained
/// schedule — to notice a message this very frame wrote, for no benefit: chat is the
/// producer and the consumer at once for its own delivery failure.
fn send_line(line: String, outbound: Option<&mut Outbound>, log: &mut ChatLog, now: Duration) {
    let Some(frame) = outgoing_frame(&line) else {
        return;
    };
    let Some(outbound) = outbound else {
        return;
    };
    if outbound.send(frame) == Sent::Dropped {
        warn!("the outbound queue was full; one chat or party request was dropped");
        push_player_message(
            log,
            &PlayerMessage::new(
                PlayerMessageKind::Error,
                "Your message did not reach the server; try again.",
            ),
            now,
        );
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

#[allow(clippy::too_many_arguments)] // The log's selection and its spans are the eighth input.
fn render_chat(
    mode: Res<InputMode>,
    draft: Res<ChatLine>,
    log: Res<ChatLog>,
    selection: Res<LogSelection>,
    time: Res<Time<Real>>,
    mut rows: LogSpans,
    mut input: Query<&mut Text, With<ChatInput>>,
    mut spans: DraftSpans,
) {
    let visible = matches!(*mode, InputMode::Playing | InputMode::Chat);
    let now = time.elapsed();
    let drawn: Vec<(FieldPieces, Color)> = (0..LINE_COUNT)
        .map(|row| match log.0.get(row) {
            Some(line) => {
                let alpha = line_alpha(*mode, visible, now.saturating_sub(line.added));
                (
                    log_pieces(&line.text, selection.range_on(line.serial, line.text.len())),
                    message_colour(line.kind, alpha),
                )
            }
            None => (log_pieces("", None), Color::NONE),
        })
        .collect();
    for (row, slot, span, colour, background) in &mut rows {
        if let Some((piece, line_colour)) = drawn
            .get(row.0)
            .and_then(|(pieces, colour)| Some((pieces.get(slot.0)?, *colour)))
        {
            paint_span(piece, line_colour, span, colour, background);
        }
    }

    let Ok(mut input) = input.single_mut() else {
        return;
    };
    let typing = *mode == InputMode::Chat;
    if typing {
        input.0 = "> ".to_owned();
    } else {
        input.0.clear();
    }
    // Closed, the draft draws as nothing at all -- an empty line, not an unfocused one.
    let pieces = if typing {
        draft.0.pieces(true)
    } else {
        TextField::default().pieces(false)
    };
    for (slot, span, colour, background) in &mut spans {
        if let Some(piece) = pieces.get(slot.0) {
            paint_span(piece, Color::WHITE, span, colour, background);
        }
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
    use crate::ui::chat_selection::{LogPoint, monospaced_layout};
    use crate::ui::clipboard::{MemoryClipboard, TextClipboard};
    use crate::ui::text_input::{CARET, CARET_COLOUR, Modifiers, SELECTION_BACKGROUND};

    const ADVANCE: f32 = 10.0;
    const ROW_HEIGHT: f32 = 20.0;

    /// An app running only the log's pointer system, over two rows laid out the way Bevy would:
    /// each row's content box starts at x 16, row 0 at y 50 and row 1 at y 70.
    fn selection_app(lines: &[&str]) -> App {
        selection_app_at(lines, 1.0)
    }

    /// [`selection_app`] on a display with `factor` physical pixels to a logical one: the nodes,
    /// the transforms and the glyphs in physical pixels, the pointer still logical, as Bevy has them.
    fn selection_app_at(lines: &[&str], factor: f32) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(InputMode::Chat)
            .insert_resource(ButtonInput::<MouseButton>::default())
            .init_resource::<ChatLog>()
            .init_resource::<LogSelection>()
            .add_systems(Update, select_in_log);
        app.world_mut().spawn((PrimaryWindow, Window::default()));
        for (row, line) in lines.iter().enumerate() {
            app.world_mut()
                .resource_mut::<ChatLog>()
                .push((*line).to_owned(), Duration::ZERO);
            app.world_mut().spawn((
                ChatText(row),
                ComputedNode {
                    size: Vec2::new(300.0, ROW_HEIGHT) * factor,
                    inverse_scale_factor: factor.recip(),
                    ..ComputedNode::DEFAULT
                },
                UiGlobalTransform::from_translation(
                    Vec2::new(166.0, 60.0 + ROW_HEIGHT * row as f32) * factor,
                ),
                monospaced_layout(line.chars().count(), ADVANCE * factor, ROW_HEIGHT * factor),
            ));
        }
        app
    }

    /// Puts the pointer at `x` pixels into the text of `row`, halfway down it.
    fn pointer_over(app: &mut App, row: usize, x: f32) {
        pointer_at(
            app,
            Some(Vec2::new(16.0 + x, 60.0 + ROW_HEIGHT * row as f32)),
        );
    }

    fn pointer_at(app: &mut App, position: Option<Vec2>) {
        app.world_mut()
            .query_filtered::<&mut Window, With<PrimaryWindow>>()
            .single_mut(app.world_mut())
            .expect("one primary window")
            .set_cursor_position(position);
    }

    /// Holds or releases the primary button for the next frame, as `InputPlugin` would report it.
    fn primary(app: &mut App, held: bool) {
        let mut buttons = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
        buttons.clear();
        if held {
            buttons.press(MouseButton::Left);
        } else {
            buttons.release(MouseButton::Left);
        }
    }

    fn drag(app: &mut App, from: (usize, f32), to: (usize, f32)) {
        pointer_over(app, from.0, from.1);
        primary(app, true);
        app.update();
        pointer_over(app, to.0, to.1);
        primary(app, true);
        app.update();
        primary(app, false);
        app.update();
    }

    fn copied(app: &App) -> Option<String> {
        let world = app.world();
        world
            .resource::<LogSelection>()
            .text(world.resource::<ChatLog>().lines())
    }

    #[test]
    fn a_drag_over_the_log_selects_from_the_glyph_pressed_to_the_glyph_released_on() {
        let mut app = selection_app(&["Eivor: hi", "Astrid: aye"]);
        // 72 px is the left half of the eighth character, so the press is before "hi"; 58 px is
        // the right half of the sixth character of the next row, so the release is after "Astrid".
        drag(&mut app, (0, 72.0), (1, 58.0));
        assert_eq!(copied(&app).as_deref(), Some("hi\nAstrid"));
        assert!(!app.world().resource::<LogSelection>().is_dragging());

        // A press on the log clears the old selection and starts anew; dragged up and left to
        // the window's corner, out of the log altogether, it still follows the nearest row, to
        // that row's start. (Past the window's edge Bevy reports no pointer at all — below.)
        pointer_over(&mut app, 1, 58.0);
        primary(&mut app, true);
        app.update();
        assert_eq!(copied(&app), None, "a fresh press selects nothing yet");
        pointer_at(&mut app, Some(Vec2::new(2.0, 2.0)));
        primary(&mut app, true);
        app.update();
        assert_eq!(copied(&app).as_deref(), Some("Eivor: hi\nAstrid"));

        // Losing the pointer mid-drag ends the drag and keeps what was selected.
        pointer_at(&mut app, None);
        app.update();
        assert!(!app.world().resource::<LogSelection>().is_dragging());
        assert_eq!(copied(&app).as_deref(), Some("Eivor: hi\nAstrid"));
    }

    /// **The pointer is taken to physical pixels before it is hit-tested.** At a factor of one the
    /// two spaces coincide, so a conversion that multiplied instead of divided — or skipped the
    /// step — would pass every other test here and put the selection in the wrong place on any
    /// scaled display. The same logical drag must select the same glyphs whatever the factor.
    #[test]
    fn a_drag_selects_the_same_glyphs_on_a_scaled_display() {
        for factor in [2.0, 1.5] {
            let mut app = selection_app_at(&["Eivor: hi", "Astrid: aye"], factor);
            drag(&mut app, (0, 72.0), (1, 58.0));
            assert_eq!(
                copied(&app).as_deref(),
                Some("hi\nAstrid"),
                "at a scale factor of {factor}"
            );
        }
    }

    #[test]
    fn a_click_elsewhere_a_line_leaving_or_closing_chat_clears_the_selection() {
        let mut app = selection_app(&["Eivor: hi", "Astrid: aye"]);
        drag(&mut app, (0, 0.0), (1, 58.0));
        assert!(copied(&app).is_some());
        pointer_at(&mut app, Some(Vec2::new(800.0, 400.0)));
        primary(&mut app, true);
        app.update();
        assert_eq!(
            *app.world().resource::<LogSelection>(),
            LogSelection::default()
        );

        // A new line that pushes nothing out leaves it alone; the one that pushes its first
        // line out of the ring drops it.
        primary(&mut app, false);
        drag(&mut app, (0, 0.0), (1, 58.0));
        for number in 2..LINE_COUNT {
            app.world_mut()
                .resource_mut::<ChatLog>()
                .push(number.to_string(), Duration::ZERO);
        }
        app.update();
        assert_eq!(copied(&app).as_deref(), Some("Eivor: hi\nAstrid"));
        app.world_mut()
            .resource_mut::<ChatLog>()
            .push("the ninth".to_owned(), Duration::ZERO);
        app.update();
        assert_eq!(copied(&app), None, "its first line left the log");

        drag(&mut app, (0, 0.0), (1, 58.0));
        assert!(copied(&app).is_some());
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert_eq!(copied(&app), None, "closing chat clears it");

        drag(&mut app, (0, 0.0), (1, 58.0));
        assert_eq!(
            *app.world().resource::<LogSelection>(),
            LogSelection::default(),
            "and nothing is selectable while chat is closed"
        );
    }

    #[test]
    fn control_c_copies_the_log_selection_and_otherwise_the_drafts() {
        let mut app = capture_app(None);
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ControlLeft);
        app.insert_resource(keys)
            .insert_resource(TextClipboard::with(MemoryClipboard::default()));
        {
            let mut log = app.world_mut().resource_mut::<ChatLog>();
            log.push("Eivor: well met".to_owned(), Duration::ZERO);
            log.push("Astrid: to the hall".to_owned(), Duration::ZERO);
        }
        {
            let mut selection = app.world_mut().resource_mut::<LogSelection>();
            selection.begin(LogPoint { line: 0, byte: 7 });
            selection.extend(LogPoint { line: 1, byte: 6 });
            selection.release();
        }
        app.world_mut()
            .resource_mut::<ChatLine>()
            .0
            .set_text("draft");

        type_key(&mut app, Key::Character("c".into()));
        app.update();
        let pasted = |app: &mut App| app.world_mut().resource_mut::<TextClipboard>().paste();
        assert_eq!(pasted(&mut app).as_deref(), Some("well met\nAstrid"));
        assert_eq!(
            app.world().resource::<ChatLine>().0.text(),
            "draft",
            "the draft is left as it was"
        );

        app.world_mut().resource_mut::<LogSelection>().clear();
        type_key(&mut app, Key::Character("a".into()));
        type_key(&mut app, Key::Character("C".into()));
        app.update();
        assert_eq!(
            pasted(&mut app).as_deref(),
            Some("draft"),
            "with nothing selected in the log, Control+C is the draft's again"
        );
    }

    #[test]
    fn a_selected_stretch_of_a_log_line_is_drawn_on_the_selection_background() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(InputMode::Chat)
            .init_resource::<ChatLine>()
            .init_resource::<ChatLog>()
            .init_resource::<LogSelection>()
            .add_systems(Startup, spawn_chat)
            .add_systems(Update, render_chat);
        app.world_mut()
            .resource_mut::<ChatLog>()
            .push("Eivor: hi".to_owned(), Duration::ZERO);
        {
            let mut selection = app.world_mut().resource_mut::<LogSelection>();
            selection.begin(LogPoint { line: 0, byte: 0 });
            selection.extend(LogPoint { line: 0, byte: 5 });
        }
        app.update();

        let row = |app: &mut App, wanted: usize| {
            let mut spans: Vec<(usize, String, Color, Color)> = app
                .world_mut()
                .query::<(
                    &LogSpan,
                    &FieldSpan,
                    &TextSpan,
                    &TextColor,
                    &TextBackgroundColor,
                )>()
                .iter(app.world())
                .filter(|span| span.0.0 == wanted)
                .map(|(_, slot, span, colour, background)| {
                    (slot.0, span.0.clone(), colour.0, background.0)
                })
                .collect();
            spans.sort_by_key(|span| span.0);
            spans
        };
        let drawn = row(&mut app, 0);
        let texts: Vec<&str> = drawn.iter().map(|span| span.1.as_str()).collect();
        assert_eq!(texts, ["", "Eivor", "", ": hi"]);
        assert_eq!(drawn[1].3, SELECTION_BACKGROUND);
        assert_eq!(drawn[3].3, Color::NONE);
        assert_eq!(
            drawn[1].2,
            message_colour(LogKind::Player, 1.0),
            "selected text keeps its line's colour"
        );
        assert!(row(&mut app, 1).iter().all(|span| span.1.is_empty()));

        app.world_mut().resource_mut::<LogSelection>().clear();
        app.update();
        let drawn = row(&mut app, 0);
        assert_eq!(drawn[0].1, "Eivor: hi");
        assert!(drawn.iter().all(|span| span.3 == Color::NONE));
    }

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

    fn capture_app(outbound: Option<Outbound>) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<KeyboardInput>()
            .insert_resource(InputMode::Chat)
            .init_resource::<ChatLine>()
            .init_resource::<ChatHistory>()
            .init_resource::<ChatLog>()
            .init_resource::<LogSelection>()
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
            app.world().resource::<ChatLine>().0.text().len(),
            DRAFT_LIMIT_BYTES
        );
    }

    /// **`Control+V` reaches the draft only while chat owns the keyboard.** Over the settings
    /// screen the mode is `Menu`, and a paste there must not land in a draft nobody can see
    /// that the next `T` would then open with.
    #[test]
    fn a_paste_lands_in_the_draft_only_while_chat_is_capturing() {
        let mut app = capture_app(None);
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ControlLeft);
        app.insert_resource(keys)
            .insert_resource(TextClipboard::with(MemoryClipboard::holding(
                "X 40 | Z -12",
            )));

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
        app.update();
        type_key(&mut app, Key::Character("v".into()));
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0.text(), "");

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        type_key(&mut app, Key::Character("v".into()));
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0.text(), "X 40 | Z -12");
    }

    /// The held modifiers reach the field: `Shift` with an arrow selects rather than moves.
    #[test]
    fn shift_held_while_an_arrow_is_pressed_selects_in_the_draft() {
        let mut app = capture_app(None);
        type_key(&mut app, Key::Character("hello".into()));
        app.update();

        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);
        app.insert_resource(keys);
        type_key(&mut app, Key::ArrowLeft);
        type_key(&mut app, Key::ArrowLeft);
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0.selection(), Some(3..5));
    }

    fn field_with(text: &str, keys: &[Key], modifiers: Modifiers) -> TextField {
        let mut field = TextField::default();
        field.set_text(text);
        for key in keys {
            press_on(&mut field, key.clone(), modifiers);
        }
        field
    }

    fn press_on(field: &mut TextField, key: Key, modifiers: Modifiers) {
        let press = KeyboardInput {
            key_code: KeyCode::KeyA,
            logical_key: key,
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        };
        field.apply_key(&press, modifiers, None, DRAFT_LIMIT_BYTES);
    }

    #[test]
    fn the_draft_is_drawn_with_a_caret_and_a_highlighted_selection_only_while_typing() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(InputMode::Chat)
            .insert_resource(ChatLine(field_with(
                "hello",
                &[const { Key::ArrowLeft }; 2],
                Modifiers {
                    shift: true,
                    control: false,
                },
            )))
            .init_resource::<ChatLog>()
            .init_resource::<LogSelection>()
            .add_systems(Startup, spawn_chat)
            .add_systems(Update, render_chat);
        app.update();

        let drawn = |app: &mut App| {
            let mut spans: Vec<(usize, String, Color, Color)> = app
                .world_mut()
                .query_filtered::<(&FieldSpan, &TextSpan, &TextColor, &TextBackgroundColor), With<DraftSpan>>()
                .iter(app.world())
                .map(|(slot, span, colour, background)| {
                    (slot.0, span.0.clone(), colour.0, background.0)
                })
                .collect();
            spans.sort_by_key(|span| span.0);
            spans
        };
        let spans = drawn(&mut app);
        let texts: Vec<&str> = spans.iter().map(|span| span.1.as_str()).collect();
        assert_eq!(texts, ["hel", CARET, "lo", ""]);
        assert_eq!(spans[1].2, CARET_COLOUR, "the caret has its own colour");
        assert_eq!(
            spans[2].3, SELECTION_BACKGROUND,
            "the selection is highlighted"
        );
        assert_eq!(spans[0].3, Color::NONE, "and the rest of the line is not");

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert!(
            drawn(&mut app).iter().all(|span| span.1.is_empty()),
            "no caret is left behind once chat closes"
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
        assert_eq!(app.world().resource::<ChatLine>().0.text(), "");
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn a_dropped_send_reaches_the_log_as_one_error_line() {
        // A zero-capacity `sync_channel` is a rendezvous: `try_send` succeeds only while a
        // receiver is parked in a blocking `recv`, which nothing here ever is, so the send
        // reads as `Sent::Dropped`. The receiver has to outlive this test — dropping it
        // would disconnect the channel and turn the send into the silent `Sent::Closed`.
        let (outbound, _never_received) = Outbound::to_a_test(0);
        let mut app = capture_app(Some(outbound));
        type_key(&mut app, Key::Character("hello".into()));
        type_key(&mut app, Key::Enter);
        app.update();

        let line = app.world().resource::<ChatLog>().0.back().unwrap();
        assert_eq!(
            line.text,
            "[ERROR] Your message did not reach the server; try again."
        );
        assert_eq!(line.kind, LogKind::System(PlayerMessageKind::Error));
    }

    fn reopen_chat(app: &mut App) {
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0.text(), "");
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
        assert_eq!(app.world().resource::<ChatLine>().0.text(), "hell!");

        for _ in 0..5 {
            type_key(&mut app, Key::Backspace);
        }
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0.text(), "");
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
        assert_eq!(
            app.world().resource::<ChatLine>().0.text(),
            "/teleport 1 2 3  "
        );
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
        assert_eq!(app.world().resource::<ChatLine>().0.text(), "remember me");
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
            .init_resource::<LogSelection>()
            .add_systems(Update, capture_chat);
        type_key(&mut app, Key::Character("t".into()));
        app.update();
        assert_eq!(app.world().resource::<ChatLine>().0.text(), "");

        assert!(line_alpha(InputMode::Playing, true, Duration::ZERO) > 0.99);
        assert!(line_alpha(InputMode::Playing, true, Duration::from_millis(11_900)) > 0.0);
        assert_eq!(line_alpha(InputMode::Playing, true, LINE_LIFETIME), 0.0);
        assert_eq!(
            line_alpha(InputMode::Chat, true, Duration::from_secs(60)),
            1.0
        );
    }
}
