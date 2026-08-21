//! The signed ticket this client keeps between launches.
//!
//! One file per account service, written with exactly the discipline
//! `net/session.rs` keeps for the identity file: mode `0600`, temporary file and
//! rename, and a path derived from the address the credential is meaningful to.
//! The atomic write is *reused* from there rather than copied — two files in one
//! data directory written the same way is one discipline, and a second copy of
//! that function would be a second discipline the first time either was edited.
//!
//! ## What is in it, and what is deliberately not
//!
//! A ticket and the second it stops working. Nothing else: not the account id, not
//! the display name. A display name is personal data and a client that never draws
//! one has no reason to hold a copy of it on disk, and the account id is the
//! server's business — see the criterion `client/AGENTS.md` states for the identity
//! file, which this follows: *nothing is decided from it*.
//!
//! **The expiry is a courtesy and never an authority.** It decides one thing — do
//! we open a browser — and the account service and the game server both re-read
//! the ticket's own signed expiry whatever this file says. A cache that lied would
//! cost a refused handshake, which is the same shape as any other refusal.
//!
//! ## No header, no version, and that follows the file beside it
//!
//! The record is fixed-width, so there is no length to declare and nothing to
//! truncate — the same dividend `internal/ticket` records for the ticket body. A
//! file of any other length is not a cache and is ignored, exactly as a
//! wrong-length identity file is not a token. If the record ever changes shape it
//! changes length, every file on disk reads as "not a ticket", and the cost is one
//! browser tab. That is a materially smaller loss than the identity file's, which
//! is why *that* file gets a paragraph about never being overwritten and this one
//! does not.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::codec::{SESSION_TICKET_LEN, SessionTicket};
use super::session::{Environment, data_home, identity_file_name, write_atomically};

/// Where cached tickets live under the data directory, one file per service.
///
/// Beside `voxelheim/identity/` rather than inside it: an identity file is one
/// server's memory of a character, and a ticket is one account service's answer
/// about a person. Two kinds of credential in one directory would make the
/// wrong-length rule above ambiguous.
const TICKET_DIR: &[&str] = &["voxelheim", "account"];

/// How many bytes the expiry occupies in the record: an `i64` of Unix seconds,
/// little-endian, which is what every other record in this repository is.
const EXPIRES_AT_LEN: usize = 8;

/// The whole record: a ticket and the second it stops working.
const RECORD_LEN: usize = SESSION_TICKET_LEN + EXPIRES_AT_LEN;

/// How many characters `session_ticket` arrives as: unpadded base64url of
/// [`SESSION_TICKET_LEN`] bytes. `ticket.Ticket.Encode` is the other half.
const ENCODED_TICKET_LEN: usize = 128;

/// A ticket read back from disk, or just obtained.
///
/// `Debug` is derived and stays safe because [`SessionTicket`]'s own `Debug`
/// redacts — which is the whole point of that newtype, and why this struct needs
/// no hand-written formatter of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CachedTicket {
    ticket: SessionTicket,
    /// The Unix second at which it stops working. Exclusive, matching
    /// `internal/ticket`: a ticket is refused at exactly this instant.
    expires_at: i64,
}

impl CachedTicket {
    pub(super) const fn new(ticket: SessionTicket, expires_at: i64) -> Self {
        Self { ticket, expires_at }
    }

    /// Whether this ticket is still worth presenting at `now`.
    pub(super) const fn is_live(&self, now: i64) -> bool {
        now < self.expires_at
    }

    /// The ticket itself.
    ///
    /// Nothing in this issue reads it — the screen that presents a ticket is the
    /// server list, and that is #107 — so this exists for the writer and for the
    /// tests that assert a round trip. It is `pub(super)` and no further, which is
    /// the same fence `PlayerToken` sits behind: a name nothing outside `net` can
    /// spell is a name nothing outside `net` can start deciding from.
    #[allow(dead_code)]
    pub(super) const fn ticket(&self) -> SessionTicket {
        self.ticket
    }
}

/// The moment this machine thinks it is, in Unix seconds.
///
/// A wall clock rather than a monotonic one, because it is compared against an
/// instant the account service named. A clock that is wrong makes a live ticket
/// look dead (one browser tab) or a dead one look live (one refusal); neither is
/// something this client can improve on by guessing.
pub(super) fn now_unix() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        // A clock set before 1970. Every ticket is then in the future, which is the
        // direction that costs a refusal rather than a bypass.
        Err(_) => i64::MIN,
    }
}

/// Where the ticket for `authority` is kept when nothing overrides it:
/// `$XDG_DATA_HOME/voxelheim/account/<authority>`, falling back to
/// `$HOME/.local/share`.
///
/// One file per account service, for the reason there is one identity file per
/// game server: a ticket is signed by one service's key and means nothing to
/// another, so B's answer must not land in A's file. The name is reduced by
/// `session::identity_file_name`, which is the function that already decides which
/// addresses reduce to a safe file name and which are refused.
pub(super) fn default_ticket_path(authority: &str, env: &Environment) -> Option<PathBuf> {
    let mut path = data_home(env)?;
    path.extend(TICKET_DIR);
    path.push(identity_file_name(authority)?);
    Some(path)
}

/// Reads the ticket in `path`, if there is one to read.
///
/// The second half of the pair is a line for the log, present exactly when
/// something was ignored — the same shape `IdentityFile::open` uses, and returned
/// rather than logged because nothing below `net/mod.rs` has a logger.
///
/// **Every failure is "no ticket", including a file that will not open.** That is
/// the one place this deliberately differs from the identity file, which refuses
/// to overwrite bytes nobody has read: a lost identity is a lost character, where
/// a lost ticket is a browser tab. Refusing to replace an unreadable cache would
/// buy nothing and would strand a player behind a file they cannot delete.
pub(super) fn read(path: &Path) -> (Option<CachedTicket>, Option<String>) {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (None, None),
        Err(err) => {
            return (
                None,
                Some(format!(
                    "cannot read the saved sign-in in {}: {err}; signing in again",
                    path.display()
                )),
            );
        }
    };

    if bytes.len() != RECORD_LEN {
        // The length, never the bytes: this text goes to a log.
        return (
            None,
            Some(format!(
                "the saved sign-in in {} holds {} bytes rather than {RECORD_LEN}; ignoring it and \
                 signing in again",
                path.display(),
                bytes.len()
            )),
        );
    }

    let mut ticket = [0u8; SESSION_TICKET_LEN];
    ticket.copy_from_slice(&bytes[..SESSION_TICKET_LEN]);
    let mut expiry = [0u8; EXPIRES_AT_LEN];
    expiry.copy_from_slice(&bytes[SESSION_TICKET_LEN..]);

    (
        Some(CachedTicket {
            ticket: SessionTicket::from_bytes(ticket),
            expires_at: i64::from_le_bytes(expiry),
        }),
        None,
    )
}

/// Replaces `path` with `cached`, or leaves it exactly as it was.
///
/// `write_atomically` is `net/session.rs`'s, which is where the temporary file,
/// the flush, the rename and the `0600` all live. A ticket is a bearer credential
/// and so wants precisely the mode the identity file wants, for precisely the same
/// reason.
pub(super) fn write(path: &Path, cached: CachedTicket) -> Result<(), String> {
    let mut record = Vec::with_capacity(RECORD_LEN);
    record.extend_from_slice(cached.ticket.as_bytes());
    record.extend_from_slice(&cached.expires_at.to_le_bytes());
    write_atomically(path, &record)
        .map_err(|err| format!("cannot save the sign-in in {}: {err}", path.display()))
}

/// Reads a ticket back from the encoding the account service answers with:
/// unpadded base64url, [`ENCODED_TICKET_LEN`] characters.
///
/// The mirror of `ticket.Decode` on the other side, and hand-rolled for the reason
/// everything else in this corner is: the dependency budget. It is narrow on
/// purpose — one alphabet, no padding, one length — so there is no general decoder
/// here for anything else to start relying on.
///
/// **The input is not echoed in the error.** It is a bearer credential.
pub(super) fn decode_ticket(encoded: &str) -> Result<SessionTicket, String> {
    if encoded.len() != ENCODED_TICKET_LEN {
        return Err(format!(
            "the account service answered a ticket of {} characters rather than {ENCODED_TICKET_LEN}",
            encoded.len()
        ));
    }

    let mut out = [0u8; SESSION_TICKET_LEN];
    let mut written = 0usize;
    // 128 characters is 32 groups of four, each of which is three bytes. The length
    // check above is what makes that exact rather than a case to handle.
    for group in encoded.as_bytes().chunks(4) {
        let mut accumulator = 0u32;
        for character in group {
            let value = base64url_value(*character).ok_or_else(|| {
                "the account service answered a ticket that is not base64url".to_owned()
            })?;
            accumulator = (accumulator << 6) | u32::from(value);
        }
        out[written] = u8::try_from((accumulator >> 16) & 0xFF).unwrap_or_default();
        out[written + 1] = u8::try_from((accumulator >> 8) & 0xFF).unwrap_or_default();
        out[written + 2] = u8::try_from(accumulator & 0xFF).unwrap_or_default();
        written += 3;
    }

    Ok(SessionTicket::from_bytes(out))
}

/// The six bits one base64url character stands for.
const fn base64url_value(character: u8) -> Option<u8> {
    match character {
        b'A'..=b'Z' => Some(character - b'A'),
        b'a'..=b'z' => Some(character - b'a' + 26),
        b'0'..=b'9' => Some(character - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::session::Scratch;

    fn ticket(byte: u8) -> SessionTicket {
        SessionTicket::from_bytes([byte; SESSION_TICKET_LEN])
    }

    /// The unpadded base64url of `bytes`, which is what the service answers with.
    fn encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for group in bytes.chunks(3) {
            let mut accumulator = 0u32;
            for (index, byte) in group.iter().enumerate() {
                accumulator |= u32::from(*byte) << (16 - 8 * index);
            }
            for index in 0..group.len() + 1 {
                let sextet = usize::try_from((accumulator >> (18 - 6 * index)) & 0x3F)
                    .expect("six bits is an index");
                out.push(char::from(ALPHABET[sextet]));
            }
        }
        out
    }

    #[test]
    fn a_ticket_round_trips_through_the_file() {
        let scratch = Scratch::new("ticket-cache");
        let path = scratch.join("service");
        let cached = CachedTicket::new(ticket(0x5a), 1_787_565_794);

        assert_eq!(write(&path, cached), Ok(()));
        let (read_back, complaint) = read(&path);
        assert_eq!(read_back, Some(cached));
        assert_eq!(complaint, None);
        assert_eq!(read_back.expect("a ticket").ticket(), ticket(0x5a));
    }

    #[test]
    fn the_file_is_created_readable_only_by_its_owner() {
        let scratch = Scratch::new("ticket-mode");
        let path = scratch.join("service");
        write(&path, CachedTicket::new(ticket(1), 1)).expect("a written ticket");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("the file").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "a ticket is a bearer credential");
        }
    }

    #[test]
    fn a_missing_file_is_a_first_sign_in_and_not_a_complaint() {
        let scratch = Scratch::new("ticket-missing");
        assert_eq!(read(&scratch.join("nothing")), (None, None));
    }

    #[test]
    fn a_wrong_length_file_is_not_a_ticket_and_says_so_without_its_bytes() {
        let scratch = Scratch::new("ticket-short");
        let path = scratch.join("service");
        let secret = b"sup3rsecretbytesthatarenotaticket";
        fs::write(&path, secret).expect("a scratch file");

        let (read_back, complaint) = read(&path);
        assert_eq!(read_back, None);
        let complaint = complaint.expect("a complaint");
        assert!(complaint.contains("bytes rather than"), "{complaint}");
        assert!(
            !complaint.contains("sup3rsecret"),
            "a complaint must not carry the file's bytes: {complaint}"
        );
    }

    #[test]
    fn a_writes_replacement_leaves_no_temporary_behind() {
        let scratch = Scratch::new("ticket-replace");
        let path = scratch.join("service");
        write(&path, CachedTicket::new(ticket(1), 10)).expect("first");
        write(&path, CachedTicket::new(ticket(2), 20)).expect("second");

        assert_eq!(read(&path).0, Some(CachedTicket::new(ticket(2), 20)));
        let strays: Vec<_> = fs::read_dir(scratch.join("."))
            .expect("the scratch directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }

    #[test]
    fn liveness_is_exclusive_at_the_instant_it_expires() {
        let cached = CachedTicket::new(ticket(3), 100);
        assert!(cached.is_live(99));
        assert!(
            !cached.is_live(100),
            "a ticket is refused at exactly its expiry"
        );
        assert!(!cached.is_live(101));
    }

    #[test]
    fn the_path_is_derived_the_way_the_identity_files_is() {
        let scratch = Scratch::new("ticket-path");
        let env = scratch.environment();
        let path = default_ticket_path("127.0.0.1:7780", &env).expect("a path");
        assert!(
            path.ends_with("voxelheim/account/127.0.0.1_7780"),
            "{path:?}"
        );

        // Case folds, exactly as it does for a server address: one endpoint is one
        // file however the operator typed it.
        assert_eq!(
            default_ticket_path("Example.Invalid:7780", &env),
            default_ticket_path("example.invalid:7780", &env)
        );
        // And an address that does not reduce to a safe name is no file at all,
        // rather than an invented escaping scheme.
        assert_eq!(default_ticket_path("../escape:1", &env), None);
    }

    #[test]
    fn a_ticket_decodes_from_the_encoding_the_service_answers_with() {
        let bytes: Vec<u8> = (0..SESSION_TICKET_LEN)
            .map(|index| u8::try_from(index * 7 % 251).expect("under 251"))
            .collect();
        let mut expected = [0u8; SESSION_TICKET_LEN];
        expected.copy_from_slice(&bytes);
        let encoded = encode(&bytes);

        assert_eq!(encoded.len(), ENCODED_TICKET_LEN);
        assert_eq!(
            decode_ticket(&encoded),
            Ok(SessionTicket::from_bytes(expected))
        );
    }

    #[test]
    fn the_whole_alphabet_decodes() {
        // `-` and `_` are the two characters that separate base64url from base64,
        // and a decoder that got them wrong would fail on roughly one ticket in
        // sixteen rather than on all of them.
        let mut bytes = [0u8; SESSION_TICKET_LEN];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::try_from(index * 3 % 256).unwrap_or(0);
        }
        let encoded = encode(&bytes);
        assert!(encoded.contains('-') || encoded.contains('_'), "{encoded}");
        assert_eq!(
            decode_ticket(&encoded),
            Ok(SessionTicket::from_bytes(bytes))
        );
    }

    #[test]
    fn a_ticket_of_the_wrong_shape_is_refused_without_being_quoted() {
        let secret = "s".repeat(ENCODED_TICKET_LEN - 1);
        let err = decode_ticket(&secret).expect_err("too short");
        assert!(!err.contains(&secret), "{err}");

        let mut wrong = "A".repeat(ENCODED_TICKET_LEN);
        wrong.replace_range(0..1, "+");
        let err = decode_ticket(&wrong).expect_err("not base64url");
        assert!(!err.contains(&wrong), "{err}");
        assert!(err.contains("base64url"), "{err}");
    }

    #[test]
    fn a_cached_ticket_prints_nothing_of_itself() {
        // The whole reason the ticket is a newtype: a `{:?}` of anything holding one
        // must not put a bearer credential in a log line.
        let printed = format!("{:?}", CachedTicket::new(ticket(0xAB), 5));
        assert!(printed.contains("<redacted>"), "{printed}");
        assert!(!printed.contains("171"), "{printed}");
    }
}
