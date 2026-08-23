//! The list a player clicks instead of an address they type.
//!
//! One read of `GET /v1/servers` on the account service, on its own `std::thread` for
//! the reason `net/signin.rs` runs on one: it blocks, and it owns a socket. No Bevy
//! type crosses the line — it speaks in [`ServerListEvent`] over `std::sync::mpsc`, and
//! the ECS drains with `try_recv` and never waits.
//!
//! ## What this list is for, and what it replaces
//!
//! Two things, and the second is the security half:
//!
//! - **The address.** It is read on every launch, so a server that moved is followed
//!   without anybody being told a new one. Nothing here remembers an address between
//!   launches; there is no cache to go stale and no file to correct.
//! - **The certificate fingerprint**, which is what this client verifies the server
//!   against. It replaces trust on first use outright — see `net/tls.rs` — rather than
//!   sitting beside it, because two ways to decide who a server is means the weaker one
//!   decides whenever the stronger is unavailable.
//!
//! **So an unreadable list is a refusal, never a shorter list.** A read that fails
//! produces [`ServerListEvent::Unavailable`], which the screen renders as "the login
//! service could not be reached" with a retry on it. It never produces an empty list:
//! an empty list is a true statement — *no server has registered* — and answering a
//! network failure with it would be this client stating something it does not know.
//! The same rule applies one level down, to a single malformed row: a list is refused
//! whole rather than quietly shortened, because a row this client drops is a server a
//! player is told does not exist.
//!
//! ## The credential
//!
//! The list is behind a ticket — it is people's home addresses, which is why the
//! account service put it there — so this presents the cached one as
//! `Authorization: Bearer …`. **The ticket is read from its file here, on this thread,
//! and never reaches the ECS**, which is the fence `net/mod.rs` already describes: a
//! session reads the identity file the same way. Nothing below logs, and no error here
//! quotes a ticket, an address or a body.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use super::http;
use super::json;
use super::signin::AccountService;
use super::tickets;
use super::tls;

/// How long the read may take, per phase. The same budget a sign-in POST gets.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The account service's list endpoint, relative to whatever prefix it is served
/// under. `server/cmd/voxelheim-auth/main.go` is authoritative for it.
const SERVERS_PATH: &str = "/v1/servers";

/// One row of the list, as this client holds it.
///
/// **The address and the fingerprint have no public accessor, deliberately.** `ui`
/// renders a name and whether the server is reachable, and asks to join by *name*; the
/// network boundary is what turns that name into an address and an expectation. A UI
/// that could read an address is a UI that could put one on screen or into a log, and
/// an address locates somebody's house — which is the whole reason the account service
/// keeps this list behind a credential.
#[derive(Clone, PartialEq, Eq)]
pub struct ListedServer {
    name: String,
    display_name: String,
    address: String,
    fingerprint: String,
    online: bool,
}

impl ListedServer {
    /// The registry's own name for this server, which is what a [`super::ConnectRequest`]
    /// carries and what the account service mints a world ticket for.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The title a player reads.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Whether the account service has heard from this server recently.
    ///
    /// **Not a reachability probe**, and the list screen says so in as many words:
    /// nothing in this client or in the account service dials a game server to find
    /// out. It is "this server announced itself inside the registry's window", which is
    /// the honest thing to show and is why an offline row stays clickable.
    pub fn online(&self) -> bool {
        self.online
    }

    /// Where to dial. `pub(super)` and no further — see the type's own comment.
    pub(super) fn address(&self) -> &str {
        &self.address
    }

    /// What certificate to expect there. `pub(super)` for the same reason, and built
    /// through [`tls::Expectation`] so the one shape a session can be started with is
    /// one that carries a fingerprint somebody stated.
    pub(super) fn expectation(&self) -> tls::Expectation {
        tls::Expectation::Listed(self.fingerprint.clone())
    }

    /// One row, as [`read_list`] would have built it.
    ///
    /// Test-only, and it exists so `net/mod.rs` can drive a click against a stub
    /// server and `ui/servers.rs` can draw a panel, neither of them with an account
    /// service to read a list from — the same seam `NetPlugin::as_if_listed` is. The
    /// fingerprint is the shape the registry admits and is never checked over the
    /// plaintext transport those tests use.
    #[cfg(test)]
    pub fn for_a_test(name: &str, address: &str, online: bool) -> Self {
        Self {
            name: name.to_owned(),
            display_name: name.to_owned(),
            address: address.to_owned(),
            fingerprint: "0".repeat(tls::FINGERPRINT_CHARS),
            online,
        }
    }
}

/// Prints the row a player sees and **not the address**.
///
/// Hand-written for the reason `PlayerToken` and `SessionTicket` write their own: the
/// redaction is then a property of the type rather than a habit every `{:?}` has to
/// remember. The fingerprint is public by construction — the server logs it at startup
/// — but it is left out too, because nothing reading a log line about the list has a
/// use for it and a shorter line is a line somebody reads.
impl std::fmt::Debug for ListedServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListedServer")
            .field("name", &self.name)
            .field("display_name", &self.display_name)
            .field("online", &self.online)
            .field("address", &"<redacted>")
            .finish()
    }
}

/// What the list thread tells the ECS.
///
/// Both variants are terminal: one read, one answer, and the thread ends. Asking again
/// is a new thread, which is what the retry on the screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServerListEvent {
    /// The list, exactly as the account service ordered it. May be empty, which means
    /// no server has registered — a true statement, and a different one from a failure.
    Ready(Vec<ListedServer>),
    /// The list could not be read. The screen shows this line and offers a retry.
    Unavailable(String),
    /// The cached ticket is no longer good. Its own variant because the answer is
    /// different in kind: there is nothing to retry until the player signs in again,
    /// so the login screen comes back up rather than the list offering a button that
    /// cannot work.
    SignedOut(String),
}

/// Reads the list once and reports what happened.
///
/// Runs on its own thread. Every failure is reported as an event and then returned
/// from — the thread never panics, because a panicking list thread would take down a
/// client that could otherwise have shown the player what went wrong.
pub(super) fn run(
    service: AccountService,
    ticket_path: Option<PathBuf>,
    events: &Sender<ServerListEvent>,
) {
    // A closed channel means the app is already gone, which is not a failure.
    let _ = events.send(fetch(&service, ticket_path.as_deref()));
}

/// One read, as the event it becomes.
fn fetch(service: &AccountService, ticket_path: Option<&Path>) -> ServerListEvent {
    let Some(path) = ticket_path else {
        return ServerListEvent::SignedOut(
            "no file could be named to keep a sign-in in, so there is no ticket to read the \
             server list with. Sign in again."
                .to_owned(),
        );
    };

    // The complaint is deliberately dropped rather than shown: `tickets::read` already
    // names the file and the shape, and the *answer* a player needs here is the same
    // either way — sign in again. `SignInPlugin` logs that complaint when it reads the
    // same file at startup.
    let (cached, _complaint) = tickets::read(path);
    let Some(cached) = cached.filter(|cached| cached.is_live(tickets::now_unix())) else {
        return ServerListEvent::SignedOut(SIGN_IN_AGAIN.to_owned());
    };

    let credential = tickets::encode_ticket(&cached.ticket());
    let response = match http::get_json(
        service.transport(),
        service.authority(),
        &servers_path(service),
        &credential,
        REQUEST_TIMEOUT,
    ) {
        Ok(response) => response,
        Err(err) => return ServerListEvent::Unavailable(err),
    };

    if response.status != 200 {
        return refusal(&response);
    }

    match read_list(&response.body) {
        Ok(servers) => ServerListEvent::Ready(servers),
        Err(err) => ServerListEvent::Unavailable(err),
    }
}

/// What a player is told when the ticket they hold will not do.
const SIGN_IN_AGAIN: &str = "Your last sign-in has expired. Sign in again to play.";

/// The list endpoint under whatever prefix this service is served at.
fn servers_path(service: &AccountService) -> String {
    format!("{}{SERVERS_PATH}", service.prefix())
}

/// Reads the whole document, or refuses it.
///
/// **Refused whole, never shortened.** A row this client cannot read is a row it cannot
/// verify a server against, and dropping it would tell a player that somebody's server
/// does not exist. The account service validates every field at registration, so a
/// malformed row means the two sides disagree about the shape of the list — which is
/// exactly the thing a refusal should surface and a tolerant reader would hide.
fn read_list(body: &str) -> Result<Vec<ListedServer>, String> {
    let document = json::parse_object(body)?;
    let rows = document.objects("servers")?;

    let mut servers = Vec::with_capacity(rows.len());
    for row in rows {
        let name = row.string("name")?.to_owned();
        // Absent is the account service's own default rather than an error: it fills a
        // missing display name with the name at registration, so a row without one is a
        // row from a service that has not been restarted, not a broken row.
        let display_name = match row.optional_string("display_name") {
            Some(display) if !display.trim().is_empty() => display.to_owned(),
            _ => name.clone(),
        };
        let address = row.string("address")?.to_owned();
        let raw = row.string("certificate_sha256")?;
        let Some(fingerprint) = tls::parse_fingerprint(raw) else {
            // The *name* is named and the fingerprint is not. A name is public — it is
            // what a player reads off the screen — and quoting the value would put a
            // field somebody typed into a log line for no gain: the operator's fix is
            // to register the server again, and this says which one.
            return Err(format!(
                "the server list carries no usable certificate fingerprint for {name}, so \
                 nothing in it can be verified. Ask whoever runs the account service to \
                 register that server again."
            ));
        };

        servers.push(ListedServer {
            name,
            display_name,
            address,
            fingerprint,
            online: row.boolean("online")?,
        });
    }
    Ok(servers)
}

/// Turns the account service's refusal into a line a player reads.
///
/// The codes are the closed set `server/cmd/voxelheim-auth/servers.go` defines, and the
/// same rule holds as for a sign-in: a refusal never carries a word that came from a
/// request, which is what makes it safe to show. An unrecognised one is shown as itself
/// — it is still from that closed set, just from a newer copy of it.
fn refusal(response: &http::Response) -> ServerListEvent {
    let code = json::parse_object(&response.body)
        .ok()
        .and_then(|fields| fields.optional_string("error").map(str::to_owned));

    match code.as_deref() {
        // The one refusal that is not a fault and not a retry: the ticket ran out, so
        // the answer is the login screen rather than a button that would fail again.
        Some("ticket_expired") => ServerListEvent::SignedOut(SIGN_IN_AGAIN.to_owned()),
        // Every other way of getting the credential wrong arrives as one code, which is
        // the account service refusing to say which guesses are getting warmer. From
        // here it is still "sign in again": whatever this client holds, it is not
        // something that service will accept.
        Some("unauthorized") => ServerListEvent::SignedOut(
            "the account service did not accept this sign-in. Sign in again.".to_owned(),
        ),
        Some("registry_unavailable") => ServerListEvent::Unavailable(
            "the account service could not read its list of servers.".to_owned(),
        ),
        Some(other) => ServerListEvent::Unavailable(format!(
            "the account service would not answer with the server list: {other}"
        )),
        None => ServerListEvent::Unavailable(format!(
            "the account service answered {} and nothing this client could read",
            response.status
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::codec::{SESSION_TICKET_LEN, SessionTicket};

    /// A fingerprint of the shape the registry admits, distinguishable per server.
    fn digest(byte: char) -> String {
        std::iter::repeat_n(byte, tls::FINGERPRINT_CHARS).collect()
    }

    fn body(rows: &str) -> String {
        format!("{{\"servers\":[{rows}],\"offline_after_seconds\":90}}")
    }

    fn row(name: &str, fingerprint: &str, online: bool) -> String {
        format!(
            "{{\"name\":\"{name}\",\"display_name\":\"{name} Hall\",\
             \"address\":\"server.example:7777\",\"certificate_sha256\":\"{fingerprint}\",\
             \"online\":{online},\"last_seen\":\"2026-08-21T10:03:14Z\"}}"
        )
    }

    /// The shape the account service actually answers with, read end to end: two
    /// servers, in order, each with what a player reads and what a session needs.
    #[test]
    fn a_list_of_two_servers_is_read_in_order() {
        let document = body(&format!(
            "{},{}",
            row("midgard", &digest('a'), true),
            row("asgard", &digest('b'), false)
        ));

        let servers = read_list(&document).expect("a readable list");
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name(), "midgard");
        assert_eq!(servers[0].display_name(), "midgard Hall");
        assert!(servers[0].online());
        assert_eq!(servers[0].address(), "server.example:7777");
        assert_eq!(
            servers[0].expectation(),
            tls::Expectation::Listed(digest('a'))
        );
        assert_eq!(servers[1].name(), "asgard");
        assert!(!servers[1].online(), "an offline server is still listed");
    }

    /// **An empty list is an answer, not a failure.** A registry nobody has registered
    /// with says `[]`, and reading that as an error would put a retry in front of a
    /// player whose account service is working perfectly.
    #[test]
    fn an_empty_registry_reads_as_an_empty_list() {
        assert_eq!(read_list(&body("")), Ok(Vec::new()));
    }

    /// A row whose fingerprint is not a digest takes the **whole** list down. Dropping
    /// it would tell a player that somebody's server does not exist, which is the one
    /// thing this screen must never say by accident.
    #[test]
    fn a_row_with_no_usable_fingerprint_refuses_the_whole_list() {
        let document = body(&format!(
            "{},{}",
            row("midgard", &digest('a'), true),
            row("asgard", "not-a-digest", true)
        ));

        let err = read_list(&document).expect_err("a list with an unverifiable row was accepted");
        assert!(err.contains("asgard"), "{err}");
        // The name is enough to act on; the value that was wrong is not echoed.
        assert!(!err.contains("not-a-digest"), "{err}");
    }

    /// A body that is not the document this endpoint answers with is refused rather
    /// than partly read — the same rule `net/json.rs` keeps one layer down.
    #[test]
    fn a_body_that_is_not_the_list_is_refused() {
        for document in [
            "{}",
            "{\"servers\":\"none\"}",
            "{\"servers\":[1,2]}",
            "{\"servers\":[{\"name\":\"midgard\"}]}",
            "not json at all",
        ] {
            assert!(read_list(document).is_err(), "{document}");
        }
    }

    /// A display name the service left out falls back to the name rather than to an
    /// empty row: the account service applies that default itself, so a row without one
    /// is an older service rather than a nameless server.
    #[test]
    fn a_missing_display_name_falls_back_to_the_name() {
        let document = format!(
            "{{\"servers\":[{{\"name\":\"midgard\",\"address\":\"server.example:7777\",\
             \"certificate_sha256\":\"{}\",\"online\":true}}]}}",
            digest('a')
        );
        let servers = read_list(&document).expect("a readable list");
        assert_eq!(servers[0].display_name(), "midgard");
    }

    /// An expired ticket sends the player back to the login screen rather than offering
    /// a retry that cannot work — the distinction the account service split
    /// `ticket_expired` out of `unauthorized` to make possible.
    #[test]
    fn an_expired_ticket_asks_for_a_new_sign_in() {
        let expired = refusal(&http::Response {
            status: 401,
            body: "{\"error\":\"ticket_expired\"}".to_owned(),
        });
        assert!(matches!(expired, ServerListEvent::SignedOut(_)));

        let refused = refusal(&http::Response {
            status: 401,
            body: "{\"error\":\"unauthorized\"}".to_owned(),
        });
        assert!(matches!(refused, ServerListEvent::SignedOut(_)));

        let broken = refusal(&http::Response {
            status: 500,
            body: "{\"error\":\"registry_unavailable\"}".to_owned(),
        });
        assert!(matches!(broken, ServerListEvent::Unavailable(_)));

        // A refusal this client has never heard of is still shown, and is still not an
        // empty list.
        let unknown = refusal(&http::Response {
            status: 503,
            body: "{\"error\":\"something_new\"}".to_owned(),
        });
        assert!(matches!(unknown, ServerListEvent::Unavailable(_)));
    }

    /// A client with no ticket at all asks for a sign-in rather than reading an empty
    /// list — the failure this whole module is written to avoid.
    #[test]
    fn a_read_with_no_ticket_is_never_an_empty_list() {
        let service = AccountService::plaintext("http://127.0.0.1:7780").expect("a service");
        assert!(matches!(
            fetch(&service, None),
            ServerListEvent::SignedOut(_)
        ));
    }

    /// The endpoint sits under whatever prefix the service is served at, which is the
    /// same derivation the two sign-in paths use.
    #[test]
    fn the_path_follows_the_services_prefix() {
        let root = AccountService::plaintext("http://127.0.0.1:7780").expect("a service");
        assert_eq!(servers_path(&root), "/v1/servers");

        let prefixed =
            AccountService::plaintext("http://accounts.example/voxelheim/").expect("a URL");
        assert_eq!(servers_path(&prefixed), "/voxelheim/v1/servers");
    }

    /// A `{:?}` of a row must not put somebody's home address in a log line. The
    /// account service goes to the same trouble on its own side, and for the same
    /// reason: it is the one field in this document that locates a person.
    #[test]
    fn a_row_prints_nothing_that_locates_anybody() {
        let servers = read_list(&body(&row("midgard", &digest('a'), true))).expect("a list");
        let printed = format!("{:?}", servers[0]);
        assert!(printed.contains("midgard"), "{printed}");
        assert!(
            !printed.contains("server.example"),
            "a server's address reached a log line: {printed}"
        );
    }

    /// The credential is presented in the encoding the account service reads back, and
    /// the round trip is what pins the two halves to each other.
    #[test]
    fn a_cached_ticket_is_presented_as_the_service_wrote_it() {
        let bytes: [u8; SESSION_TICKET_LEN] =
            std::array::from_fn(|index| u8::try_from(index * 5 % 251).expect("under 251") ^ 0x3C);
        let ticket = SessionTicket::from_bytes(bytes);

        let encoded = tickets::encode_ticket(&ticket);
        assert_eq!(tickets::decode_ticket(&encoded), Ok(ticket));
    }
}
