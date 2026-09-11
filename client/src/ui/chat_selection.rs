//! Selecting text in the chat log with the pointer, and the string a copy of that selection is.
//!
//! **The log is read-only, so this is a selection and not a field.** The draft beneath it is a
//! [`super::text_input::TextField`], with a cursor that typing moves; nothing types into the log, so
//! what it needs is two points and the rule for turning them into text. The draft keeps the
//! keyboard throughout: a drag here changes which log text `Control+C` copies and nothing else.
//!
//! **A point names a line by the order it arrived in, never by the row it is drawn on.** The log
//! is a ring of eight: a new line pushes the oldest out and every other line moves up a row. A
//! selection kept by row would silently slide onto the line below it whenever anybody spoke, so it
//! is kept by [`LogPoint::line`] — the count of lines the log had taken before that one — and a
//! byte offset into that line's displayed text. A selection one of whose lines has left the ring
//! is dropped whole rather than cut down to what remains, because half a quoted exchange copied
//! without a word of warning is worse than a drag that has to be made again.
//!
//! **Where a character is drawn is Bevy's answer, not arithmetic over a font size.** Bevy UI lays
//! each line out into [`TextLayoutInfo`], and [`glyph_cells`] reads the pen position of every glyph
//! back out of it — the one place the laid-out geometry is interpreted. Everything below that is
//! plain numbers, which is what lets the hit-testing be tested headlessly.

use std::ops::Range;

use bevy::math::{Rect, Vec2};
use bevy::prelude::Resource;
use bevy::text::TextLayoutInfo;

/// One place in the log: a line and a byte offset into its displayed text.
///
/// Ordered by line and then by byte, which is reading order, so the earlier of two points is
/// simply the smaller one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct LogPoint {
    /// How many lines the log had taken before this one; stable while the line is in the log.
    pub(super) line: u64,
    /// A byte offset on a character boundary of that line's text.
    pub(super) byte: usize,
}

/// What is selected in the log, and whether the button that is selecting it is still held.
///
/// **The anchor is where the press landed and the focus is where the pointer is now**, so dragging
/// back past the anchor selects the other way instead of losing the start. Anchor and focus equal
/// is a click that has not moved yet, and it selects nothing.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct LogSelection {
    ends: Option<(LogPoint, LogPoint)>,
    dragging: bool,
}

impl LogSelection {
    /// A press on the log: nothing selected yet, and the drag that will select begins here.
    pub(super) fn begin(&mut self, at: LogPoint) {
        self.ends = Some((at, at));
        self.dragging = true;
    }

    /// The pointer moved with the button held; the selection now reaches `to`.
    pub(super) fn extend(&mut self, to: LogPoint) {
        if self.dragging
            && let Some((_, focus)) = &mut self.ends
        {
            *focus = to;
        }
    }

    /// The button came up, or the pointer was lost: what is selected stays selected.
    pub(super) fn release(&mut self) {
        self.dragging = false;
    }

    /// Selects nothing and ends any drag.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether a drag started on the log is still under way.
    pub(super) const fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// The selection's start and end in reading order, or `None` when nothing is selected.
    fn bounds(&self) -> Option<(LogPoint, LogPoint)> {
        let (anchor, focus) = self.ends?;
        (anchor != focus).then(|| (anchor.min(focus), anchor.max(focus)))
    }

    /// The selected bytes of `line`, whose text is `len` bytes long, when any of it is selected.
    pub(super) fn range_on(&self, line: u64, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.bounds()?;
        if line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line {
            start.byte.min(len)
        } else {
            0
        };
        let to = if line == end.line {
            end.byte.min(len)
        } else {
            len
        };
        (from < to).then_some(from..to)
    }

    /// Drops the selection if a line it touches is older than `oldest`, the oldest line the log
    /// still holds — or if the log holds nothing at all.
    pub(super) fn forget_lines_before(&mut self, oldest: Option<u64>) {
        let Some((anchor, focus)) = self.ends else {
            return;
        };
        let first = anchor.line.min(focus.line);
        if oldest.is_none_or(|oldest| first < oldest) {
            self.clear();
        }
    }

    /// The selected text of `lines`, one line per selected log line joined with `\n`, exactly as
    /// the lines are displayed — a player's line keeps the `Name: ` it was shown with.
    ///
    /// `None` when nothing is selected or the selection covers no characters, so a copy of it is
    /// never a copy that empties the clipboard.
    pub(super) fn text<'a>(
        &self,
        lines: impl IntoIterator<Item = (u64, &'a str)>,
    ) -> Option<String> {
        let (start, end) = self.bounds()?;
        let pieces: Vec<&str> = lines
            .into_iter()
            .filter(|(line, _)| (start.line..=end.line).contains(line))
            .map(|(line, text)| {
                self.range_on(line, text.len())
                    .map_or("", |range| &text[range])
            })
            .collect();
        let joined = pieces.join("\n");
        (!joined.trim_matches('\n').is_empty()).then_some(joined)
    }
}

/// Where one character of a laid-out line was drawn, in the text block's own physical pixels.
///
/// `left` is the glyph's pen position and `right` the next glyph's on the same row, so the cells of
/// a row tile it with no gap: a pointer between two characters is always over one of them. `top`
/// and `bottom` are the row's line box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GlyphCell {
    /// The row of the laid-out block, for a line long enough to wrap.
    pub(super) row: usize,
    pub(super) left: f32,
    pub(super) right: f32,
    pub(super) top: f32,
    pub(super) bottom: f32,
}

/// Reads one cell per glyph out of Bevy's layout of a line, in text order.
///
/// **The pen position is recovered exactly.** Bevy stores a glyph at the centre of its atlas image,
/// `pen + size / 2 + offset`, which puts a space — an empty image — half a character left of where a
/// letter would be; undoing that sum gives back the pen, which is where the character's advance
/// begins whatever its image looks like.
///
/// **A glyph is taken to be a character.** The log is drawn in `default_font`, a monospaced ASCII
/// subset, and every received line has been through `bounded_display`, so each character shapes to
/// one glyph. A line that broke that — a ligature, a combining mark — would have its later carets
/// land a character early; [`byte_of`] clamps rather than panics, so the worst it can do is select
/// slightly less than was dragged over.
pub(super) fn glyph_cells(layout: &TextLayoutInfo) -> Vec<GlyphCell> {
    let pens: Vec<Vec2> = layout
        .glyphs
        .iter()
        .map(|glyph| glyph.position - glyph.atlas_info.rect.size() / 2.0 - glyph.atlas_info.offset)
        .collect();
    layout
        .glyphs
        .iter()
        .zip(&pens)
        .enumerate()
        .map(|(index, (glyph, &pen))| {
            // A run is one row's worth of one span, so the run a pen sits in names this glyph's
            // line box. Half a pixel in from the pen, so a glyph at a span boundary is found in
            // the span it starts rather than the one that ends there.
            let run = layout
                .run_geometry
                .iter()
                .map(|run| run.bounds)
                .find(|bounds| bounds.contains(Vec2::new(pen.x + 0.5, pen.y)));
            let next_on_row = layout.glyphs[index + 1..]
                .iter()
                .zip(&pens[index + 1..])
                .find(|(next, _)| next.line_index == glyph.line_index)
                .map(|(_, next)| next.x);
            let size = glyph.atlas_info.rect.size();
            GlyphCell {
                row: glyph.line_index,
                left: pen.x,
                right: next_on_row
                    .or(run.map(|bounds| bounds.max.x))
                    .unwrap_or(pen.x + size.x),
                top: run.map_or(glyph.position.y - size.y / 2.0, |bounds| bounds.min.y),
                bottom: run.map_or(glyph.position.y + size.y / 2.0, |bounds| bounds.max.y),
            }
        })
        .collect()
}

/// Which caret position a point is at: the number of characters before it on its line.
///
/// The row is the one whose line box holds the point, or the nearest one when none does — a drag
/// that leaves the text above or below still selects to the start or the end of a row. Within the
/// row the caret goes before the first character whose middle is right of the point, so a press on
/// the left half of a character selects from before it and one on the right half from after it.
pub(super) fn caret_at(cells: &[GlyphCell], point: Vec2) -> usize {
    let Some(row) = cells
        .iter()
        .min_by(|a, b| distance_to_row(a, point.y).total_cmp(&distance_to_row(b, point.y)))
        .map(|cell| cell.row)
    else {
        return 0;
    };
    let mut after_row = 0;
    for (index, cell) in cells.iter().enumerate().filter(|(_, cell)| cell.row == row) {
        if point.x < (cell.left + cell.right) / 2.0 {
            return index;
        }
        after_row = index + 1;
    }
    after_row
}

fn distance_to_row(cell: &GlyphCell, y: f32) -> f32 {
    if y < cell.top {
        cell.top - y
    } else if y > cell.bottom {
        y - cell.bottom
    } else {
        0.0
    }
}

/// The byte offset of the `ordinal`th character of `text`, or its length past the last one.
pub(super) fn byte_of(text: &str, ordinal: usize) -> usize {
    text.char_indices()
        .nth(ordinal)
        .map_or(text.len(), |(byte, _)| byte)
}

/// One log line as it is drawn this frame.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DrawnLine<'a> {
    pub(super) line: u64,
    pub(super) text: &'a str,
    /// The line's content box in physical window pixels.
    pub(super) frame: Rect,
    pub(super) cells: Vec<GlyphCell>,
}

/// The log point under `pointer`, in physical window pixels.
///
/// `inside` asks for a press: only a point within a drawn line's box answers, so a click anywhere
/// else is a click elsewhere. Without it — a drag already under way — the nearest line answers, so
/// the pointer may leave the log and keep the selection following it to the top or bottom line.
pub(super) fn point_at(lines: &[DrawnLine<'_>], pointer: Vec2, inside: bool) -> Option<LogPoint> {
    let line = if inside {
        lines.iter().find(|line| line.frame.contains(pointer))?
    } else {
        lines.iter().min_by(|a, b| {
            distance_to_frame(a.frame, pointer.y).total_cmp(&distance_to_frame(b.frame, pointer.y))
        })?
    };
    let caret = caret_at(&line.cells, pointer - line.frame.min);
    Some(LogPoint {
        line: line.line,
        byte: byte_of(line.text, caret),
    })
}

fn distance_to_frame(frame: Rect, y: f32) -> f32 {
    if y < frame.min.y {
        frame.min.y - y
    } else if y > frame.max.y {
        y - frame.max.y
    } else {
        0.0
    }
}

/// Bevy's layout of a one-row line of `characters` monospaced glyphs, `advance` wide and in a line
/// box `height` tall, built the way `bevy_text` builds one — for a test with no text pipeline.
#[cfg(test)]
pub(super) fn monospaced_layout(characters: usize, advance: f32, height: f32) -> TextLayoutInfo {
    use bevy::asset::AssetId;
    use bevy::text::{GlyphAtlasInfo, PositionedGlyph, RunGeometry};

    let size = Vec2::new(advance * 0.6, height * 0.45);
    let offset = Vec2::new(advance * 0.2, -size.y);
    let glyphs = (0..characters)
        .map(|column| {
            let pen = Vec2::new(advance * column as f32, height * 0.75);
            PositionedGlyph {
                position: size / 2.0 + pen + offset,
                atlas_info: GlyphAtlasInfo {
                    texture: AssetId::default(),
                    rect: Rect::from_corners(Vec2::ZERO, size),
                    offset,
                    is_alpha_mask: true,
                },
                section_index: 1,
                line_index: 0,
            }
        })
        .collect();
    TextLayoutInfo {
        glyphs,
        run_geometry: vec![RunGeometry {
            section_index: 1,
            bounds: Rect::new(0.0, 0.0, advance * characters as f32, height),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bevy::asset::AssetId;
    use bevy::text::{GlyphAtlasInfo, PositionedGlyph, RunGeometry};

    #[test]
    fn the_test_layout_is_hit_where_its_characters_are_drawn() {
        let cells = glyph_cells(&monospaced_layout(4, 10.0, 20.0));
        let lefts: Vec<f32> = cells.iter().map(|cell| cell.left).collect();
        assert_eq!(lefts, [0.0, 10.0, 20.0, 30.0]);
        assert_eq!(cells[3].right, 40.0);
        assert_eq!(caret_at(&cells, Vec2::new(26.0, 10.0)), 3);
    }

    /// Ten pixels a character and twenty a row, which is all a monospaced line is.
    const ADVANCE: f32 = 10.0;
    const ROW: f32 = 20.0;

    /// Cells for rows of the given lengths, the way a wrapped line is laid out.
    fn rows(lengths: &[usize]) -> Vec<GlyphCell> {
        let mut cells = Vec::new();
        for (row, &length) in lengths.iter().enumerate() {
            for column in 0..length {
                let left = ADVANCE * column as f32;
                cells.push(GlyphCell {
                    row,
                    left,
                    right: left + ADVANCE,
                    top: ROW * row as f32,
                    bottom: ROW * (row + 1) as f32,
                });
            }
        }
        cells
    }

    fn point(line: u64, byte: usize) -> LogPoint {
        LogPoint { line, byte }
    }

    fn dragged(from: LogPoint, to: LogPoint) -> LogSelection {
        let mut selection = LogSelection::default();
        selection.begin(from);
        selection.extend(to);
        selection.release();
        selection
    }

    #[test]
    fn a_point_takes_the_caret_before_or_after_the_character_by_its_half() {
        let cells = rows(&[5]);
        let at = |x: f32| caret_at(&cells, Vec2::new(x, 10.0));
        assert_eq!(at(-30.0), 0, "left of the line is its start");
        assert_eq!(
            at(4.9),
            0,
            "the left half of the first character is before it"
        );
        assert_eq!(at(5.1), 1, "and its right half after it");
        assert_eq!(
            at(10.0),
            1,
            "the boundary itself is inside the second's left half"
        );
        assert_eq!(at(44.9), 4);
        assert_eq!(
            at(45.1),
            5,
            "the right half of the last character is the end"
        );
        assert_eq!(at(400.0), 5, "and so is anywhere past it");
        assert_eq!(
            caret_at(&[], Vec2::new(40.0, 10.0)),
            0,
            "an empty line has one caret"
        );
    }

    #[test]
    fn a_wrapped_line_is_hit_on_the_row_the_point_is_in_or_nearest() {
        // "abcde" then "fgh": the second row starts at the sixth character.
        let cells = rows(&[5, 3]);
        assert_eq!(
            caret_at(&cells, Vec2::new(12.0, 25.0)),
            6,
            "second row, after f"
        );
        assert_eq!(
            caret_at(&cells, Vec2::new(12.0, 5.0)),
            1,
            "first row, after a"
        );
        assert_eq!(
            caret_at(&cells, Vec2::new(90.0, 30.0)),
            8,
            "past a short row is its end"
        );
        assert_eq!(
            caret_at(&cells, Vec2::new(0.0, -50.0)),
            0,
            "above is the top row"
        );
        assert_eq!(
            caret_at(&cells, Vec2::new(90.0, 99.0)),
            8,
            "below is the bottom row"
        );
        assert_eq!(
            caret_at(&cells, Vec2::new(12.0, ROW)),
            1,
            "a point on the line between two rows belongs to the upper one"
        );
    }

    #[test]
    fn a_caret_becomes_a_byte_on_a_character_boundary() {
        // "a", a two-byte e-acute, a three-byte euro.
        let text = "a\u{e9}\u{20ac}";
        let bytes: Vec<usize> = (0..=5).map(|ordinal| byte_of(text, ordinal)).collect();
        assert_eq!(bytes, [0, 1, 3, 6, 6, 6], "past the end clamps to the end");
    }

    /// A glyph built the way Bevy builds one: stored at its image's centre, `pen + size/2 + offset`.
    fn glyph(pen: Vec2, size: Vec2, offset: Vec2, line_index: usize) -> PositionedGlyph {
        PositionedGlyph {
            position: size / 2.0 + pen + offset,
            atlas_info: GlyphAtlasInfo {
                texture: AssetId::default(),
                rect: Rect::from_corners(Vec2::ZERO, size),
                offset,
                is_alpha_mask: true,
            },
            section_index: 1,
            line_index,
        }
    }

    fn run(min: Vec2, max: Vec2) -> RunGeometry {
        RunGeometry {
            section_index: 1,
            bounds: Rect::from_corners(min, max),
            ..Default::default()
        }
    }

    #[test]
    fn glyph_cells_recover_the_pen_so_a_space_is_as_wide_as_a_letter() {
        // "a b" on one row, baseline at 15, then "c" wrapped onto a second row.
        let letter = Vec2::new(6.0, 9.0);
        let layout = TextLayoutInfo {
            glyphs: vec![
                glyph(Vec2::new(0.0, 15.0), letter, Vec2::new(2.0, -9.0), 0),
                glyph(Vec2::new(10.0, 15.0), Vec2::ZERO, Vec2::ZERO, 0),
                glyph(Vec2::new(20.0, 15.0), letter, Vec2::new(1.0, -9.0), 0),
                glyph(Vec2::new(0.0, 35.0), letter, Vec2::new(2.0, -9.0), 1),
            ],
            run_geometry: vec![
                run(Vec2::new(0.0, 0.0), Vec2::new(30.0, 20.0)),
                run(Vec2::new(0.0, 20.0), Vec2::new(10.0, 40.0)),
            ],
            ..Default::default()
        };
        let cells = glyph_cells(&layout);
        let edges: Vec<(usize, f32, f32, f32, f32)> = cells
            .iter()
            .map(|cell| (cell.row, cell.left, cell.right, cell.top, cell.bottom))
            .collect();
        assert_eq!(
            edges,
            [
                (0, 0.0, 10.0, 0.0, 20.0),
                (0, 10.0, 20.0, 0.0, 20.0),
                (0, 20.0, 30.0, 0.0, 20.0),
                (1, 0.0, 10.0, 20.0, 40.0),
            ],
            "the last cell of a row ends where its run does"
        );
        assert_eq!(
            caret_at(&cells, Vec2::new(12.0, 10.0)),
            1,
            "the left half of the space is before it, not after it"
        );
    }

    #[test]
    fn a_drag_selects_in_reading_order_whichever_way_it_went() {
        let forward = dragged(point(3, 2), point(5, 4));
        let backward = dragged(point(5, 4), point(3, 2));
        for selection in [&forward, &backward] {
            assert_eq!(selection.range_on(2, 10), None, "before the selection");
            assert_eq!(
                selection.range_on(3, 10),
                Some(2..10),
                "the first line from its point"
            );
            assert_eq!(
                selection.range_on(4, 10),
                Some(0..10),
                "a middle line whole"
            );
            assert_eq!(
                selection.range_on(5, 10),
                Some(0..4),
                "the last line to its point"
            );
            assert_eq!(selection.range_on(6, 10), None, "after the selection");
        }
        assert!(
            !forward.is_dragging(),
            "a release ends the drag and keeps the text"
        );

        let click = dragged(point(3, 2), point(3, 2));
        assert_eq!(
            click.range_on(3, 10),
            None,
            "a click that did not move selects nothing"
        );
    }

    #[test]
    fn copied_text_is_each_line_as_displayed_joined_with_newlines() {
        let log = [
            (7, "Eivor: well met"),
            (8, "[INFO] The storm is coming"),
            (9, "Astrid: to the hall"),
        ];
        let selection = dragged(point(7, 7), point(9, 6));
        assert_eq!(
            selection.text(log).as_deref(),
            Some("well met\n[INFO] The storm is coming\nAstrid"),
        );
        assert_eq!(
            dragged(point(9, 0), point(9, 6)).text(log).as_deref(),
            Some("Astrid"),
            "one line copies without a newline"
        );
        assert_eq!(
            dragged(point(8, 30), point(9, 0)).text(log),
            None,
            "the end of one line to the start of the next covers no character"
        );
        assert_eq!(LogSelection::default().text(log), None);
    }

    #[test]
    fn a_selection_is_dropped_once_any_line_it_touches_leaves_the_log() {
        let mut selection = dragged(point(3, 1), point(5, 2));
        selection.forget_lines_before(Some(2));
        selection.forget_lines_before(Some(3));
        assert_eq!(
            selection.range_on(4, 10),
            Some(0..10),
            "an older line leaving moves nothing"
        );

        selection.forget_lines_before(Some(4));
        assert_eq!(
            selection,
            LogSelection::default(),
            "its first line left, so all of it goes"
        );

        let mut pressed = LogSelection::default();
        pressed.begin(point(3, 1));
        pressed.forget_lines_before(Some(4));
        assert!(
            !pressed.is_dragging(),
            "a press on a line that left ends the drag too"
        );

        let mut emptied = dragged(point(3, 1), point(3, 4));
        emptied.forget_lines_before(None);
        assert_eq!(emptied, LogSelection::default());
    }

    #[test]
    fn a_press_needs_a_line_under_it_and_a_drag_follows_the_nearest_line() {
        let drawn = |line: u64, text: &'static str, top: f32| DrawnLine {
            line,
            text,
            frame: Rect::from_corners(Vec2::new(100.0, top), Vec2::new(400.0, top + ROW)),
            cells: rows(&[text.len()]),
        };
        let lines = [drawn(4, "first", 500.0), drawn(5, "second", 520.0)];

        assert_eq!(
            point_at(&lines, Vec2::new(122.0, 530.0), true),
            Some(point(5, 2)),
            "the log's frame is subtracted before the cells are asked"
        );
        assert_eq!(
            point_at(&lines, Vec2::new(122.0, 480.0), true),
            None,
            "above the log"
        );
        assert_eq!(
            point_at(&lines, Vec2::new(50.0, 505.0), true),
            None,
            "left of it"
        );

        assert_eq!(
            point_at(&lines, Vec2::new(900.0, 100.0), false),
            Some(point(4, 5)),
            "dragged far above and right, the top line to its end"
        );
        assert_eq!(
            point_at(&lines, Vec2::new(0.0, 900.0), false),
            Some(point(5, 0)),
            "dragged below and left, the bottom line from its start"
        );
        assert_eq!(
            point_at(&[], Vec2::ZERO, false),
            None,
            "an empty log has no point"
        );
    }
}
