//! Reading what the account service answers with.
//!
//! Two text formats arrive from it and this client has no crate for either: the
//! JSON body of a response, and the RFC 3339 timestamps carried inside it as
//! strings. Both readers here are deliberately small and deliberately strict —
//! `client/AGENTS.md` sets the dependency budget at three crates, and a fourth
//! needs a discussion rather than a commit.
//!
//! **Strict rather than tolerant, and that is the design.** A scanner that hunted
//! for `"state"` and took the next string would be a quarter of the size and would
//! be fooled by an escaped quote, a nested object, or a field whose name happens to
//! contain another field's name. [`parse_object`] instead reads a *flat* object —
//! string, number, boolean and null values, no arrays and no nesting — and refuses
//! anything else. Nothing the account service answers with is nested, so the shape
//! this refuses is a shape that would mean the service had changed; a refusal says
//! so, where a scanner would quietly return the wrong field.
//!
//! **No error here ever quotes its input.** A `finish` response carries a bearer
//! credential and a `finish` request carries an authorization code, so a message
//! built from those bytes is a message that can carry one into a log. Every refusal
//! below names the field and the shape that was wanted and nothing else — the same
//! rule `server/cmd/voxelheim-auth/signin.go` keeps on the other side, for the same
//! reason.

/// What a malformed body is reported as. One text for every position, because the
/// alternative — a message that says *where* — is a message built from the input.
const NOT_AN_OBJECT: &str = "the account service answered something that is not a JSON object";

/// One field's value.
///
/// A number is kept as the text it arrived as, because nothing in this client reads
/// one: converting would be arithmetic performed only so its result could be thrown
/// away, and it would give a malformed number a second way to fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Value {
    Str(String),
    Number(String),
    Bool(bool),
    Null,
}

/// A flat JSON object, in the order its fields arrived.
///
/// A `Vec` rather than a map: there are four or five fields, a linear scan over
/// five entries is not a cost, and preserving order keeps the value comparable in a
/// test without anybody having to think about hashing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Fields(Vec<(String, Value)>);

impl Fields {
    /// The string at `key`, or a refusal naming the key.
    pub(super) fn string(&self, key: &str) -> Result<&str, String> {
        match self.get(key) {
            Some(Value::Str(text)) => Ok(text),
            Some(_) => Err(format!("the account service answered a non-string `{key}`")),
            None => Err(format!("the account service answered no `{key}`")),
        }
    }

    /// The string at `key` when there is one, and `None` when the field is absent.
    ///
    /// The refusal path uses it: an error body is `{"error": "..."}` and a body that
    /// is not one is still an error, so the caller wants "if it said which" rather
    /// than "it must have said which".
    pub(super) fn optional_string(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(text)) => Some(text),
            _ => None,
        }
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }
}

/// Reads a flat JSON object, refusing everything else.
pub(super) fn parse_object(input: &str) -> Result<Fields, String> {
    let mut reader = Reader::new(input.as_bytes());
    reader.whitespace();
    reader.expect(b'{')?;
    let mut fields: Vec<(String, Value)> = Vec::new();

    reader.whitespace();
    if reader.peek() == Some(b'}') {
        reader.step();
    } else {
        loop {
            reader.whitespace();
            let key = reader.string()?;
            reader.whitespace();
            reader.expect(b':')?;
            reader.whitespace();
            let value = reader.value()?;
            if fields.iter().any(|(name, _)| *name == key) {
                // Refused rather than last-one-wins, for the reason `main.rs`
                // refuses an option given twice: two values for one name means one
                // of them is not what was meant, and choosing silently is how the
                // wrong one gets used.
                return Err(format!(
                    "the account service answered `{key}` twice in one object"
                ));
            }
            fields.push((key, value));
            reader.whitespace();
            match reader.next() {
                Some(b',') => continue,
                Some(b'}') => break,
                _ => return Err(NOT_AN_OBJECT.to_owned()),
            }
        }
    }

    reader.whitespace();
    if !reader.finished() {
        return Err(NOT_AN_OBJECT.to_owned());
    }
    Ok(Fields(fields))
}

/// A cursor over the body's bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn finished(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn step(&mut self) {
        self.at += 1;
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek();
        if byte.is_some() {
            self.step();
        }
        byte
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.next() == Some(byte) {
            Ok(())
        } else {
            Err(NOT_AN_OBJECT.to_owned())
        }
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.step();
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.literal(b"true").map(|()| Value::Bool(true)),
            Some(b'f') => self.literal(b"false").map(|()| Value::Bool(false)),
            Some(b'n') => self.literal(b"null").map(|()| Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number().map(Value::Number),
            // Refused rather than skipped. Stepping over a structure this reader
            // does not understand is exactly how it would go on to misread the
            // field after it.
            _ => Err(NOT_AN_OBJECT.to_owned()),
        }
    }

    fn literal(&mut self, word: &[u8]) -> Result<(), String> {
        if self.bytes[self.at..].starts_with(word) {
            self.at += word.len();
            Ok(())
        } else {
            Err(NOT_AN_OBJECT.to_owned())
        }
    }

    /// A JSON number, kept as text. The grammar is checked; the value is not read.
    fn number(&mut self) -> Result<String, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.step();
        }
        if !self.digits() {
            return Err(NOT_AN_OBJECT.to_owned());
        }
        if self.peek() == Some(b'.') {
            self.step();
            if !self.digits() {
                return Err(NOT_AN_OBJECT.to_owned());
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.step();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.step();
            }
            if !self.digits() {
                return Err(NOT_AN_OBJECT.to_owned());
            }
        }
        // Every byte consumed above is ASCII, so the slice is valid UTF-8 by
        // construction and this cannot be the failing branch.
        String::from_utf8(self.bytes[start..self.at].to_vec()).map_err(|_| NOT_AN_OBJECT.to_owned())
    }

    /// Consumes one or more digits, reporting whether there were any.
    fn digits(&mut self) -> bool {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.step();
        }
        self.at > start
    }

    /// A JSON string, with every escape the format defines.
    ///
    /// **`\uXXXX` is not optional here.** Go's encoder escapes `<`, `>` and `&` by
    /// default, so `authorize_url` — which is a query string and therefore full of
    /// `&` — arrives with `&` in it. A reader that passed the escape through
    /// would hand the browser a URL with `&` where the separators should be.
    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            match self.next().ok_or_else(|| NOT_AN_OBJECT.to_owned())? {
                b'"' => return Ok(out),
                b'\\' => {
                    let escape = self.next().ok_or_else(|| NOT_AN_OBJECT.to_owned())?;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.escaped_char()?),
                        _ => return Err(NOT_AN_OBJECT.to_owned()),
                    }
                }
                // A control character is illegal unescaped, and refusing one keeps
                // a newline out of a value that later reaches a log line.
                byte if byte < 0x20 => return Err(NOT_AN_OBJECT.to_owned()),
                byte => {
                    // Multi-byte UTF-8 arrives one byte at a time; gather the whole
                    // sequence and let the standard library judge it.
                    let width = utf8_width(byte);
                    let start = self.at - 1;
                    self.at = (start + width).min(self.bytes.len());
                    let text = std::str::from_utf8(&self.bytes[start..self.at])
                        .map_err(|_| NOT_AN_OBJECT.to_owned())?;
                    out.push_str(text);
                }
            }
        }
    }

    /// One `\uXXXX` escape, including the surrogate pair a character outside the
    /// basic plane arrives as.
    fn escaped_char(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        if (0xD800..0xDC00).contains(&first) {
            // A high surrogate is only half a character: the low half must follow,
            // as its own escape.
            self.expect(b'\\')?;
            self.expect(b'u')?;
            let second = self.hex4()?;
            if !(0xDC00..0xE000).contains(&second) {
                return Err(NOT_AN_OBJECT.to_owned());
            }
            let combined = 0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00);
            return char::from_u32(combined).ok_or_else(|| NOT_AN_OBJECT.to_owned());
        }
        char::from_u32(first).ok_or_else(|| NOT_AN_OBJECT.to_owned())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = self.next().ok_or_else(|| NOT_AN_OBJECT.to_owned())?;
            let nibble = match digit {
                b'0'..=b'9' => u32::from(digit - b'0'),
                b'a'..=b'f' => u32::from(digit - b'a') + 10,
                b'A'..=b'F' => u32::from(digit - b'A') + 10,
                _ => return Err(NOT_AN_OBJECT.to_owned()),
            };
            value = value * 16 + nibble;
        }
        Ok(value)
    }
}

/// How many bytes the UTF-8 sequence starting with `lead` occupies.
///
/// An invalid lead byte answers 1, which makes the slice above fail the UTF-8 check
/// rather than running past the end of a character.
const fn utf8_width(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// One JSON string, quoted and escaped, for a request body this client builds.
///
/// The only writing half this module has, and it stays this narrow deliberately:
/// `finish` sends three strings and nothing else, so a serialiser would be a
/// general facility built for one caller. What it must get right is the escaping —
/// a `code` is provider-chosen text, and a body that put an unescaped quote in the
/// middle of it would be a body the service cannot read.
///
/// Non-ASCII is passed through rather than escaped, which is legal JSON and what
/// every encoder does; `\u00XX` covers the control characters that are not.
pub(super) fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            control if control < '\u{20}' => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// What a malformed timestamp is reported as. Named for the same reason
/// [`NOT_AN_OBJECT`] is: the input is not echoed.
const NOT_A_TIMESTAMP: &str = "the account service answered a time this client cannot read";

/// The Unix second an RFC 3339 timestamp names.
///
/// This is the shape Go's `time.Time` marshals into a JSON string, which is where
/// every timestamp this client reads comes from:
/// `2026-08-21T10:03:14Z`, optionally with a fraction and optionally with a numeric
/// offset instead of `Z`.
///
/// **The fraction is dropped rather than rounded**, and the direction is the one to
/// be wrong in: every timestamp here is an expiry, and truncating makes a ticket
/// look like it dies a fraction of a second early. Rounding up would make a dead
/// one look alive.
pub(super) fn unix_seconds(stamp: &str) -> Result<i64, String> {
    let bytes = stamp.as_bytes();
    // `1970-01-01T00:00:00` plus a zone is the shortest legal input.
    if bytes.len() < 20 {
        return Err(NOT_A_TIMESTAMP.to_owned());
    }

    let year = number(bytes, 0, 4)?;
    separator(bytes, 4, b'-')?;
    let month = number(bytes, 5, 2)?;
    separator(bytes, 7, b'-')?;
    let day = number(bytes, 8, 2)?;
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return Err(NOT_A_TIMESTAMP.to_owned());
    }
    let hour = number(bytes, 11, 2)?;
    separator(bytes, 13, b':')?;
    let minute = number(bytes, 14, 2)?;
    separator(bytes, 16, b':')?;
    let second = number(bytes, 17, 2)?;

    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        // 60 is a leap second, which every calendar this arithmetic uses folds into
        // the following minute. Accepted and clamped rather than refused.
        || second > 60
    {
        return Err(NOT_A_TIMESTAMP.to_owned());
    }

    let mut at = 19;
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while matches!(bytes.get(at), Some(b'0'..=b'9')) {
            at += 1;
        }
        if at == start {
            return Err(NOT_A_TIMESTAMP.to_owned());
        }
    }

    let offset = match bytes.get(at) {
        Some(b'Z' | b'z') => {
            at += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            if bytes.len() < at + 6 || bytes[at + 3] != b':' {
                return Err(NOT_A_TIMESTAMP.to_owned());
            }
            let hours = number(bytes, at + 1, 2)?;
            let minutes = number(bytes, at + 4, 2)?;
            if hours > 23 || minutes > 59 {
                return Err(NOT_A_TIMESTAMP.to_owned());
            }
            at += 6;
            sign * (hours * 3600 + minutes * 60)
        }
        // A timestamp with no zone names no instant. Guessing UTC would be this
        // client deciding what somebody else's clock meant.
        _ => return Err(NOT_A_TIMESTAMP.to_owned()),
    };

    if at != bytes.len() {
        return Err(NOT_A_TIMESTAMP.to_owned());
    }

    let seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second.min(59);
    Ok(seconds - offset)
}

/// `width` decimal digits at `at`, as a number.
fn number(bytes: &[u8], at: usize, width: usize) -> Result<i64, String> {
    let slice = bytes
        .get(at..at + width)
        .ok_or_else(|| NOT_A_TIMESTAMP.to_owned())?;
    let mut value: i64 = 0;
    for digit in slice {
        if !digit.is_ascii_digit() {
            return Err(NOT_A_TIMESTAMP.to_owned());
        }
        value = value * 10 + i64::from(digit - b'0');
    }
    Ok(value)
}

fn separator(bytes: &[u8], at: usize, want: u8) -> Result<(), String> {
    if bytes.get(at) == Some(&want) {
        Ok(())
    } else {
        Err(NOT_A_TIMESTAMP.to_owned())
    }
}

const fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days from 1970-01-01 to `year-month-day`, proleptic Gregorian.
///
/// Howard Hinnant's `days_from_civil`, which is the standard closed form and is
/// exact for every year this format can hold — no loop over years, and no table.
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    // March-based years, so the leap day lands at the end and needs no special case.
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &str) -> Fields {
        parse_object(input).expect("a flat JSON object")
    }

    #[test]
    fn a_start_response_reads_field_by_field() {
        let fields = parsed(
            r#"{"state":"abc","finish_secret":"sec","authorize_url":"https://example.invalid/x","expires_at":"2026-08-21T10:03:14Z"}"#,
        );
        assert_eq!(fields.string("state"), Ok("abc"));
        assert_eq!(fields.string("finish_secret"), Ok("sec"));
        assert_eq!(
            fields.string("authorize_url"),
            Ok("https://example.invalid/x")
        );
        assert_eq!(fields.string("expires_at"), Ok("2026-08-21T10:03:14Z"));
    }

    #[test]
    fn a_finish_response_carries_a_boolean_beside_its_strings() {
        let fields = parsed(
            r#"{"account_id":"0f","display_name":"thora","created":true,"session_ticket":"t","ticket_expires_at":"2026-08-21T18:03:14Z"}"#,
        );
        assert_eq!(fields.string("session_ticket"), Ok("t"));
        assert_eq!(fields.get("created"), Some(&Value::Bool(true)));
    }

    #[test]
    fn the_escapes_gos_encoder_produces_are_read_back() {
        // `&` is escaped by encoding/json's default HTML escaping, which is what an
        // authorize URL is full of.
        let fields = parsed(r#"{"authorize_url":"https://example.invalid/a?b=1&c=2<d>"}"#);
        assert_eq!(
            fields.string("authorize_url"),
            Ok("https://example.invalid/a?b=1&c=2<d>")
        );
    }

    #[test]
    fn every_escape_the_format_defines_is_read() {
        let fields = parsed(r#"{"k":"\"\\\/\b\f\n\r\tA"}"#);
        assert_eq!(fields.string("k"), Ok("\"\\/\u{8}\u{c}\n\r\tA"));
    }

    #[test]
    fn a_surrogate_pair_becomes_one_character() {
        let fields = parsed(r#"{"k":"😀"}"#);
        assert_eq!(fields.string("k"), Ok("\u{1f600}"));
    }

    #[test]
    fn a_lone_high_surrogate_is_refused() {
        assert!(parse_object(r#"{"k":"\ud83d"}"#).is_err());
        assert!(parse_object(r#"{"k":"\ud83dA"}"#).is_err());
    }

    #[test]
    fn multibyte_text_survives_unescaped() {
        let fields = parsed("{\"k\":\"Þórа\u{1f600}\"}");
        assert_eq!(fields.string("k"), Ok("Þórа\u{1f600}"));
    }

    #[test]
    fn nesting_is_refused_rather_than_skipped() {
        // The refusal is the point: a reader that stepped over a value it does not
        // understand is a reader that can go on to return the wrong field.
        assert!(parse_object(r#"{"a":{"b":1},"c":"d"}"#).is_err());
        assert!(parse_object(r#"{"a":[1,2],"c":"d"}"#).is_err());
    }

    #[test]
    fn a_field_given_twice_is_refused() {
        let err = parse_object(r#"{"state":"a","state":"b"}"#).expect_err("which one?");
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn an_empty_object_is_an_object() {
        assert_eq!(parse_object("  {  }  "), Ok(Fields::default()));
    }

    #[test]
    fn a_missing_field_says_which_one() {
        let fields = parsed(r#"{"a":"b"}"#);
        let err = fields.string("state").expect_err("there is no state");
        assert!(err.contains("state"), "{err}");
        assert_eq!(fields.optional_string("state"), None);
    }

    #[test]
    fn a_non_object_body_is_refused() {
        for body in ["", "   ", "null", "[]", "\"text\"", "{", "{\"a\"}", "{}{}"] {
            assert!(parse_object(body).is_err(), "{body:?}");
        }
    }

    #[test]
    fn no_refusal_quotes_the_body_it_read() {
        // The rule this module exists to keep: a `finish` response holds a bearer
        // credential, so a message built from it is a message that can carry one.
        let secret = "sup3rsecretticketbytes";
        for body in [
            format!("{{\"session_ticket\":\"{secret}\""),
            format!("{{\"session_ticket\":\"{secret}\",\"session_ticket\":\"x\"}}"),
            format!("[\"{secret}\"]"),
            format!("{{\"a\":{{\"b\":\"{secret}\"}}}}"),
        ] {
            let err = parse_object(&body).expect_err("malformed");
            assert!(!err.contains(secret), "{err}");
        }
    }

    #[test]
    fn a_control_character_inside_a_string_is_refused() {
        assert!(parse_object("{\"k\":\"a\nb\"}").is_err());
    }

    #[test]
    fn numbers_are_read_as_text_and_never_as_arithmetic() {
        let fields = parsed(r#"{"a":0,"b":-12,"c":1.5,"d":2e10,"e":-3.25E-4}"#);
        assert_eq!(fields.get("a"), Some(&Value::Number("0".to_owned())));
        assert_eq!(fields.get("e"), Some(&Value::Number("-3.25E-4".to_owned())));
        // A leading zero is illegal JSON and this grammar takes it anyway. Stated
        // rather than fixed: nothing here reads a number, so the strictness that
        // would buy something is the strictness about *strings*, and a refusal for
        // `01` would be a refusal nothing is protected by.
        assert!(parse_object(r#"{"a":01}"#).is_ok());
        assert!(parse_object(r#"{"a":-}"#).is_err());
        assert!(parse_object(r#"{"a":1.}"#).is_err());
    }

    #[test]
    fn a_quoted_string_reads_back_as_itself() {
        for value in [
            "",
            "plain",
            "with \"quotes\"",
            "with \\ backslash",
            "with\nnewline\tand tab",
            "with \u{1}\u{1f} controls",
            "Þórа\u{1f600}",
        ] {
            let body = format!("{{\"k\":{}}}", quote(value));
            let fields = parse_object(&body).unwrap_or_else(|err| panic!("{value:?}: {err}"));
            assert_eq!(fields.string("k"), Ok(value), "{value:?}");
        }
    }

    #[test]
    fn the_epoch_is_zero() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Ok(0));
    }

    #[test]
    fn a_known_instant_matches_the_arithmetic_go_would_do() {
        // 2026-08-21T10:03:14Z, the moment the amendment comment on this issue
        // carries. Checked against `date -u -d ... +%s`.
        assert_eq!(unix_seconds("2026-08-21T10:03:14Z"), Ok(1_787_306_594));
        assert_eq!(unix_seconds("2000-02-29T12:00:00Z"), Ok(951_825_600));
        assert_eq!(unix_seconds("2100-03-01T00:00:00Z"), Ok(4_107_542_400));
    }

    #[test]
    fn a_fraction_is_dropped_rather_than_rounded() {
        assert_eq!(
            unix_seconds("2026-08-21T10:03:14.999999999Z"),
            Ok(1_787_306_594)
        );
        assert_eq!(unix_seconds("2026-08-21T10:03:14.5Z"), Ok(1_787_306_594));
    }

    #[test]
    fn an_offset_moves_the_instant() {
        let utc = unix_seconds("2026-08-21T10:03:14Z").expect("utc");
        assert_eq!(unix_seconds("2026-08-21T12:03:14+02:00"), Ok(utc));
        assert_eq!(unix_seconds("2026-08-21T08:03:14-02:00"), Ok(utc));
    }

    #[test]
    fn a_leap_second_folds_into_the_minute_that_follows() {
        assert_eq!(
            unix_seconds("2016-12-31T23:59:60Z"),
            unix_seconds("2016-12-31T23:59:59Z")
        );
    }

    #[test]
    fn a_timestamp_with_no_zone_names_no_instant() {
        assert!(unix_seconds("2026-08-21T10:03:14").is_err());
    }

    #[test]
    fn an_impossible_date_is_refused() {
        for stamp in [
            "2026-02-30T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-00-01T00:00:00Z",
            "2026-01-00T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:60:00Z",
            "2026-01-01T00:00:61Z",
            "2025-02-29T00:00:00Z",
            "2026-08-21X10:03:14Z",
            "2026-08-21T10:03:14+0200",
            "2026-08-21T10:03:14Zz",
            "2026-08-21T10:03:14.Z",
            "not-a-time",
            "",
        ] {
            assert!(unix_seconds(stamp).is_err(), "{stamp}");
        }
    }

    #[test]
    fn a_timestamp_refusal_does_not_quote_its_input() {
        let err = unix_seconds("sup3rsecret").expect_err("not a time");
        assert!(!err.contains("sup3rsecret"), "{err}");
    }
}
