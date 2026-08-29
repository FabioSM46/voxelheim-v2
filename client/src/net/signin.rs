//! One sign-in attempt: the browser tab, the loopback listener, and the two POSTs.
//!
//! This module runs on its own `std::thread` for the reason `net/session.rs` does:
//! it blocks, and it owns sockets. No Bevy type crosses the line — it speaks in
//! [`SignInEvent`] and [`SignInCommand`] over `std::sync::mpsc`, and the ECS side
//! drains with `try_recv` and never waits.
//!
//! ## The three values this client handles
//!
//! - **`state`** is public. It is minted by the account service, travels to the
//!   provider inside the authorize URL, and comes back through the browser. It is
//!   compared against the one `start` answered with **twice**: in the accept loop,
//!   so that a request from anything other than this sign-in cannot end the wait,
//!   and again **before anything is sent to `finish`**.
//! - **`finish_secret`** is private and stays in memory for the life of one
//!   attempt. It is never put in a URL, never written to a file and never logged.
//!   It exists because the provider's redirect carries `code` and `state` in the
//!   *same* URL, so a secret that travelled through the browser would protect
//!   nothing — without it, `state` is a bearer credential and whoever can read the
//!   redirect can finish somebody else's sign-in as them.
//! - **the ticket** is private, is cached at mode `0600`, and is a bearer
//!   credential exactly as an identity token is.
//!
//! **The PKCE verifier is not one of them.** The account service mints it and the
//! account service redeems the code, because PKCE requires the redeemer to hold the
//! verifier. It never exists on this machine and nothing here talks to the
//! provider's token endpoint.
//!
//! ## Where the listener binds, and why it is not an ephemeral port
//!
//! The redirect URI is the account service's configuration — it is registered with
//! the provider, and the provider will send the browser there and nowhere else. So
//! this client reads it out of the `redirect_uri` inside the authorize URL and
//! binds *exactly that*, after checking it is loopback and plain HTTP. A listener
//! on a port of its own choosing would be a listener the browser never reaches —
//! **including the one the operating system would choose.** A redirect URI naming
//! port 0 is refused rather than bound: the browser is sent to the literal
//! `redirect_uri`, so an ephemeral port is a port nothing was told about, and
//! binding one would turn a misconfiguration into a wait that ends at the deadline
//! with nothing to say.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use super::http::{self, MAX_REQUEST_LINE_BYTES, Url};
use super::json;
use super::tickets::{self, CachedTicket};
use super::tls::{self, FINGERPRINT_CHARS};

/// How long either POST to the account service may take, per phase.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The longest this client will wait for the browser, whatever the account service
/// says its sign-in is good for. A player who walked away is not a player this
/// client should keep a socket bound for.
const MAX_BROWSER_WAIT: Duration = Duration::from_secs(300);

/// The shortest it will wait, whatever the account service says. A clock a few
/// seconds out must not turn into a sign-in that expired before the tab opened.
const MIN_BROWSER_WAIT: Duration = Duration::from_secs(30);

/// How long the accept loop sleeps between looks. Short enough that closing the
/// window does not leave a thread parked; long enough that waiting costs twenty
/// wakeups a second. The same reasoning as `session::READ_TIMEOUT`, one order
/// smaller because nothing is arriving in the meantime.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// How long one connection to the loopback listener may take to say what it wants.
///
/// Browsers open connections speculatively and then say nothing on them, so this
/// has to be short: it is how long a speculative connection can delay the redirect
/// arriving behind it.
const REQUEST_TIMEOUT_LOOPBACK: Duration = Duration::from_secs(2);

/// The account service's two endpoints, relative to whatever prefix it is served
/// under. `server/cmd/voxelheim-auth/main.go` is authoritative for both.
const START_PATH: &str = "/v1/signin/discord/start";
const FINISH_PATH: &str = "/v1/signin/discord/finish";

/// A value that must not reach a log, a file or a URL.
///
/// A newtype for the reason `PlayerToken` is one, and the mirror of
/// `discord.Secret` on the other side: [`fmt::Debug`] is written by hand and prints
/// no bytes, so the redaction is a property of the type rather than a habit every
/// call site has to remember. There is deliberately no `Display` — a secret has no
/// rendering — and the one way out is named [`Secret::reveal`], so every deliberate
/// use is one grep away.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Secret(String);

impl Secret {
    pub(super) fn new(value: String) -> Self {
        Self(value)
    }

    /// The value, for the one caller that must have it: the JSON body of `finish`.
    pub(super) fn reveal(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Where this client signs in, and **where its trust chain ends**.
///
/// Every certificate this client will accept for a game server came out of the list
/// this service answered with, and the ticket that read that list was signed by this
/// service's key. Nothing else in the client is an authority about who a server is —
/// there is no pin file any more, no second list, and no way to add a source at
/// runtime. Trust has to bottom out somewhere; this is the somewhere, and it is worth
/// being explicit rather than leaving a reader to infer it from four modules.
///
/// **What is fixed by construction is that there is exactly one of these, chosen once
/// at launch.** It is parsed in `main.rs` before a window opens — so a mistyped URL is a
/// usage error rather than a refusal on a login screen — inserted into `SignInSettings`,
/// and read from there by both the sign-in and the list. A second one cannot be
/// introduced by anything a server says.
///
/// **And the hop to it is authenticated, which is the one thing this paragraph used to
/// have to disclaim** (#131). It is `https`, and the certificate is checked against a
/// SHA-256 the launch supplied — `--account-service-fingerprint`, beside the address, the
/// way an operator hands both out. There is no root store and none is needed: pinning is
/// a digest comparison, which is what `net/tls.rs` already does for a game server. There
/// is no trust on first use, no `--insecure` and no plaintext form — first contact is
/// exactly when a substitution happens, so a fingerprint this client discovered would be
/// a fingerprint an attacker could choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountService {
    authority: String,
    /// The path the service is served under, with no trailing `/`. Empty when it is
    /// served at the root, which is what `voxelheim-auth` does on its own port.
    prefix: String,
    /// How this service is reached, carrying the certificate to expect there.
    ///
    /// **A field rather than an argument at each call, and that is the enforcement.**
    /// There is no way to hold an `AccountService` and not have stated what it must
    /// present: [`Self::parse`] is the only constructor a shipped client has and it takes
    /// the fingerprint, so "the sign-in is pinned and the list is pinned" is a property of
    /// this type rather than two call sites that each remembered.
    transport: http::Transport,
}

impl AccountService {
    /// Reads an account service URL and the fingerprint to expect at it, or says what is
    /// wrong with them.
    ///
    /// **`http` is refused rather than accepted.** The account service listens over TLS
    /// and has no plaintext form, so an `http` address cannot work — and refusing it here
    /// turns what would otherwise be a connection error on a login screen into a usage
    /// error before a window opens. It is the same refusal `cmd/voxelheimd` makes on its
    /// own copy of this address.
    ///
    /// **The fingerprint is required and is not discovered.** A malformed one is refused
    /// with the shape it should have had, because the alternative — connecting anyway and
    /// comparing against nothing — is the hole this whole path exists to close.
    pub fn parse(raw: &str, fingerprint: &str) -> Result<Self, String> {
        let url = http::parse_url(raw)?;
        match url.scheme.as_str() {
            "https" => {}
            "http" => {
                return Err(format!(
                    "{raw} is http, and the account service listens over TLS. Use https:// - and \
                     pass the fingerprint it prints when it starts, which is what this client \
                     checks instead of a certificate authority."
                ));
            }
            other => {
                return Err(format!(
                    "{raw} is {other}, and the account service speaks https"
                ));
            }
        }
        if !url.query.is_empty() {
            return Err(format!(
                "{raw} carries a query, and an account service is an address"
            ));
        }

        let Some(fingerprint) = tls::parse_fingerprint(fingerprint) else {
            return Err(format!(
                "the account service fingerprint is not a SHA-256: it is \
                 {FINGERPRINT_CHARS} hexadecimal characters, printed by that service at \
                 every start as certificate_sha256. Ask whoever runs it for the number; \
                 there is nothing this client can read it from, because this connection \
                 is where its trust begins."
            ));
        };

        Ok(Self {
            authority: url.authority(),
            prefix: url.path.trim_end_matches('/').to_owned(),
            transport: http::Transport::Pinned(fingerprint),
        })
    }

    /// The same service reached in the clear, for a test standing a fake one up.
    ///
    /// `cfg(test)` and nowhere else, which is the seam [`http::Transport`] documents: a
    /// shipped client has [`Self::parse`] and nothing else, so there is no build a player
    /// runs in which an account service can be reached unencrypted.
    #[cfg(test)]
    pub(super) fn plaintext(raw: &str) -> Result<Self, String> {
        let url = http::parse_url(raw)?;
        Ok(Self {
            authority: url.authority(),
            prefix: url.path.trim_end_matches('/').to_owned(),
            transport: http::Transport::Plaintext,
        })
    }

    /// How this service is reached, for the two modules that make requests to it.
    pub(super) fn transport(&self) -> &http::Transport {
        &self.transport
    }

    /// `host:port`, which is what the cache path is derived from and what a socket
    /// connects to.
    pub(super) fn authority(&self) -> &str {
        &self.authority
    }

    /// The path this service is served under, with no trailing `/`. Read by
    /// [`super::servers`], which adds its own route to it rather than keeping a second
    /// idea of where this service lives.
    pub(super) fn prefix(&self) -> &str {
        &self.prefix
    }

    fn start_path(&self) -> String {
        format!("{}{START_PATH}", self.prefix)
    }

    fn finish_path(&self) -> String {
        format!("{}{FINISH_PATH}", self.prefix)
    }
}

impl fmt::Display for AccountService {
    /// The address as it was given, scheme and all.
    ///
    /// The scheme comes from the transport rather than from a literal, so a line in a log
    /// cannot say `https` about a plaintext test fixture or the reverse. **The
    /// fingerprint is deliberately absent**: it is not a secret, but it is not part of
    /// the address either, and a message that carried it would be a message somebody
    /// copies the wrong half of.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = match self.transport {
            http::Transport::Pinned(_) => "https",
            #[cfg(test)]
            http::Transport::Plaintext => "http",
        };
        write!(f, "{scheme}://{}{}", self.authority, self.prefix)
    }
}

/// What the sign-in thread tells the ECS.
///
/// Two of the three are terminal. `Warning` is a line for the log and the attempt
/// carries on — the same vocabulary `SessionEvent` uses, for the same reason:
/// nothing below `net/mod.rs` has a logger, so a warning crosses as a value.
///
/// **None of them ever carries a credential.** What is written here is written to a
/// log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SignInEvent {
    /// A ticket was obtained. Whether it could also be *saved* is a `Warning`.
    Completed,
    /// There is no ticket, and this is the line a player reads on the login screen.
    Refused(String),
    /// Something worth a line in the log, and the attempt continues.
    Warning(String),
}

/// What the ECS can tell the sign-in thread.
///
/// Sent by dropping the ECS end of the channel, exactly as `NetCommand` is: "the app
/// is going away" is the only instruction there is to give.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SignInCommand {
    Cancel,
}

/// How the authorize URL reaches a browser.
///
/// **`System` is the only variant a shipped client can name.** `Captured` exists
/// under `cfg(test)` and nowhere else, which is the seam `session::Transport`
/// establishes and the reason it is safe: the tests below drive a whole sign-in
/// against a fake account service and have to play the browser's part, and a test
/// that shelled out to `xdg-open` would open a real tab on whoever ran it.
#[derive(Debug, Clone)]
pub(super) enum Browser {
    System,
    #[cfg(test)]
    Captured(Sender<String>),
}

/// Where the loopback listener comes from.
///
/// **`Bind` is the only variant a shipped client can name**, and it is the whole of
/// what production does: the redirect's port is bound here, from the `redirect_uri`
/// the account service registered, exactly as the module comment describes.
///
/// `Prebound` exists under `cfg(test)` and nowhere else, for the same reason
/// [`Browser::Captured`] does — and it closes a race the tests cannot close from
/// outside. A test has to name a concrete port before the sign-in starts, because
/// the redirect URI is built from it and a kernel-chosen port is one nothing was
/// told about. The only way to learn a free port is to bind one, and a helper that
/// binds, reads the number and *drops* the listener hands out a port that belongs
/// to nobody from that moment until the code under test binds it — so any sibling
/// test in the same binary asking the kernel for an ephemeral port can be given it,
/// and the sign-in then fails to bind, the worker returns, and the test sees its
/// channel `Disconnected`. That is #557, and it turned `develop` red on a commit
/// that touched no networking at all.
///
/// So the test keeps the listener it bound and hands *it* over instead of a bare
/// number: the port is owned continuously, by the test and then by the attempt, and
/// there is no instant at which the kernel may reissue it.
#[derive(Debug)]
pub(super) enum Loopback {
    Bind,
    #[cfg(test)]
    Prebound(TcpListener),
}

/// Runs one attempt from `start` to a cached ticket.
///
/// `world` is which ticket this attempt is asking for, and the two values are two
/// different credentials rather than one with a decoration. `None` asks for an
/// **account** ticket — one that names no world, which is what a player signs in with
/// before they have chosen one and what the server list is read with. `Some(name)` asks
/// for a **world** ticket, which is the only thing a game server admits anybody on: it
/// is `--server` with `--account-service`, where the address came from the command line
/// and so the world has to as well. See [`super::SignInPlugin`] for why nothing can
/// infer it from the address, and `net/tickets.rs` for why the two are cached apart.
///
/// Returns when the attempt ends. Every failure is reported as an event and then
/// returned from — the thread never panics, because a panicking sign-in thread
/// would take down a client that could otherwise have shown the player what went
/// wrong.
pub(super) fn run(
    service: AccountService,
    world: Option<String>,
    ticket_path: Option<PathBuf>,
    loopback: Loopback,
    browser: Browser,
    events: Sender<SignInEvent>,
    commands: Receiver<SignInCommand>,
) {
    let outcome = attempt(
        &service,
        world.as_deref(),
        ticket_path.as_deref(),
        loopback,
        &browser,
        &events,
        &commands,
    );
    let event = match outcome {
        Ok(()) => SignInEvent::Completed,
        Err(refusal) => SignInEvent::Refused(refusal),
    };
    // A closed channel means the app is already gone, which is not a failure.
    let _ = events.send(event);
}

fn attempt(
    service: &AccountService,
    world: Option<&str>,
    ticket_path: Option<&Path>,
    loopback: Loopback,
    browser: &Browser,
    events: &Sender<SignInEvent>,
    commands: &Receiver<SignInCommand>,
) -> Result<(), String> {
    let started = begin(service)?;
    let redirect = loopback_redirect(&started.authorize_url)?;
    // Bound **before** the browser is opened. The other order is a race the player
    // loses: a fast redirect would arrive at a port nothing is listening on.
    let listener = match loopback {
        Loopback::Bind => bind(&redirect)?,
        // A listener a test bound before it built the redirect URI, so the port was
        // never unowned. It has to be *that* port: a seam that quietly accepted any
        // listener would let a test pass while the browser was sent somewhere else.
        #[cfg(test)]
        Loopback::Prebound(listener) => {
            let bound = listener.local_addr().expect("a bound address").port();
            assert_eq!(
                bound, redirect.port,
                "the handed-in listener is not on the redirect's port"
            );
            listener
        }
    };

    let mut child = open_browser(browser, &started.authorize_url)?;
    let caught = wait_for_redirect(
        &listener,
        &redirect.path,
        &started.state,
        started.deadline,
        commands,
    );
    // Reaped rather than killed, and never waited on: `xdg-open` usually exits the
    // moment it has handed the URL over, but on a desktop where it falls back to
    // running the browser itself it outlives the whole attempt — and killing it
    // would close the tab the player is signing in on.
    if let Some(child) = child.as_mut() {
        let _ = child.try_wait();
    }

    let mut caught = caught?;
    let outcome = complete(
        service,
        &started,
        world,
        &caught.params,
        ticket_path,
        events,
    );
    answer_the_tab(&mut caught.stream, outcome.is_ok());
    outcome
}

/// What `start` answered with, plus the deadline it implies.
struct Started {
    state: String,
    finish_secret: Secret,
    authorize_url: String,
    /// When to stop waiting for the browser: the service's own expiry, clamped into
    /// [`MIN_BROWSER_WAIT`]..=[`MAX_BROWSER_WAIT`].
    deadline: Instant,
}

/// Asks the account service to begin a sign-in.
fn begin(service: &AccountService) -> Result<Started, String> {
    let response = http::post_json(
        service.transport(),
        service.authority(),
        &service.start_path(),
        "{}",
        REQUEST_TIMEOUT,
    )?;
    let fields = readable(&response)?;
    Ok(Started {
        state: fields.string("state")?.to_owned(),
        finish_secret: Secret::new(fields.string("finish_secret")?.to_owned()),
        authorize_url: fields.string("authorize_url")?.to_owned(),
        deadline: Instant::now() + browser_wait(fields.string("expires_at").ok()),
    })
}

/// How long to wait for the browser, given what the service said the sign-in is
/// good for.
///
/// Clamped at both ends. The service's number is the useful one — finishing after
/// it would be refused — but a machine whose clock is out, or a service that
/// answered a time this client cannot read, must not turn into a wait of zero.
fn browser_wait(expires_at: Option<&str>) -> Duration {
    let Some(expiry) = expires_at.and_then(|stamp| json::unix_seconds(stamp).ok()) else {
        // No usable expiry is a different thing from one that has passed: nothing
        // is known, so the wait is this client's own bound rather than zero.
        return MAX_BROWSER_WAIT;
    };
    // A remainder that will not fit an unsigned number is a remainder in the past,
    // which is no wait at all — and is then floored below rather than acted on, for
    // the reason the clamp exists.
    let remaining =
        u64::try_from(expiry - tickets::now_unix()).map_or(Duration::ZERO, Duration::from_secs);
    remaining.clamp(MIN_BROWSER_WAIT, MAX_BROWSER_WAIT)
}

/// The loopback address the provider will send the browser to.
///
/// Read out of the authorize URL rather than chosen here — see the module comment.
/// Both checks are refusals rather than corrections: a redirect URI that is not
/// loopback would have this client bind a public port, and one that is not plain
/// HTTP is one this listener cannot answer.
fn loopback_redirect(authorize_url: &str) -> Result<Url, String> {
    let authorize = http::parse_url(authorize_url)?;
    let redirect = http::query_pairs(&authorize.query)?
        .into_iter()
        .find(|(name, _)| name == "redirect_uri")
        .map(|(_, value)| value)
        .ok_or_else(|| {
            "the account service answered an authorize URL that names no redirect".to_owned()
        })?;

    let redirect = http::parse_url(&redirect)?;
    if redirect.scheme != "http" {
        return Err(format!(
            "the account service's redirect is {}, and this client can only answer http",
            redirect.scheme
        ));
    }
    if !is_loopback(&redirect.host) {
        return Err(format!(
            "the account service's redirect names {}, which is not a loopback address; this \
             client will not listen on one",
            redirect.host
        ));
    }
    // Port 0 would bind, and that is the trap: the browser is sent to the literal
    // `redirect_uri`, so the port the kernel picked is a port nobody was told
    // about and the redirect arrives nowhere. Said now, rather than as a wait that
    // runs to the deadline and blames the player for walking away.
    if redirect.port == 0 {
        return Err(
            "the account service's redirect names port 0, which is not a port a browser can be \
             sent to; it must name the port it is registered on"
                .to_owned(),
        );
    }
    Ok(redirect)
}

/// Whether `host` names this machine and only this machine.
fn is_loopback(host: &str) -> bool {
    match host.parse::<IpAddr>() {
        Ok(address) => address.is_loopback(),
        // The one name that is loopback by specification. Any other name would have
        // to be resolved, and a name that resolves to 127.0.0.1 today can resolve
        // somewhere else tomorrow — which is exactly the substitution this check
        // exists to refuse.
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// Binds the redirect's address, and only it.
///
/// Through [`Url::authority`] rather than a second `host:port` of its own, because
/// an IPv6 literal has to be bracketed to be an address at all and one formatter is
/// one place to get that right.
fn bind(redirect: &Url) -> Result<TcpListener, String> {
    let address = redirect.authority();
    TcpListener::bind(&address).map_err(|err| {
        format!(
            "cannot listen on {address} for the sign-in redirect: {err}. Another copy of the \
             game may already be signing in."
        )
    })
}

/// Opens the system browser at `url`, or captures it for a test.
///
/// `xdg-open` through `std::process::Command`, which is the whole of what opening a
/// browser costs on Linux and needs no dependency. Its output is discarded: a
/// launcher's diagnostics are not this client's, and the URL carries a `state`.
fn open_browser(browser: &Browser, url: &str) -> Result<Option<Child>, String> {
    match browser {
        Browser::System => Command::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Some)
            .map_err(|err| {
                format!("cannot open a browser with xdg-open: {err}. Is xdg-utils installed?")
            }),
        #[cfg(test)]
        Browser::Captured(sender) => {
            sender
                .send(url.to_owned())
                .map_err(|_| "nothing is watching for the browser".to_owned())?;
            Ok(None)
        }
    }
}

/// The redirect, and the connection it arrived on.
struct Caught {
    stream: TcpStream,
    params: Vec<(String, String)>,
}

/// Waits for the provider's redirect on the loopback listener.
///
/// **`state` is half of what identifies the redirect, and the security-relevant
/// half.** The port and the path are the account service's registered configuration
/// — public, fixed, and the same on every machine — so any page a player has open
/// can issue a request to `http://127.0.0.1:<port>/<path>` with an `<img>` tag. If
/// the path alone ended the wait, that request would *be* the redirect: `complete`
/// would refuse it (its `state` is not this attempt's, or it carries `error=…`),
/// and the listener would already be gone when the real redirect arrived a moment
/// later. That is not a way to steal a sign-in — the code and the `finish_secret`
/// are still out of reach — but it is a way for any web page to stop one, so the
/// wait ends only for a request carrying the `state` this attempt started with.
///
/// A genuine refusal from the provider carries that `state` too: RFC 6749 requires
/// the error redirect to echo it, so "the player pressed Cancel" still arrives and
/// is still reported as itself.
///
/// **Everything else is answered and the wait continues**, which is the same rule
/// that was already needed for a browser's speculative opens and favicon fetches: a
/// listener that stopped at the first connection would stop at the wrong one. A
/// query that will not even decode is in that category rather than fatal — it used
/// to end the whole attempt, which handed the same abort to anyone who could type
/// `%zz`.
fn wait_for_redirect(
    listener: &TcpListener,
    path: &str,
    state: &str,
    deadline: Instant,
    commands: &Receiver<SignInCommand>,
) -> Result<Caught, String> {
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("cannot poll the sign-in listener: {err}"))?;

    // Whether anything reached the redirect path without this attempt's `state`.
    // It changes only what the deadline says, and it is worth saying: a provider
    // that answered without echoing the `state`, or a second copy of the game
    // signing in, is a different problem from a player who walked away.
    let mut refused_a_caller = false;

    loop {
        if cancelled(commands) {
            return Err("the sign-in was stopped".to_owned());
        }
        if Instant::now() >= deadline {
            return Err(if refused_a_caller {
                "something came back to this client, but not with this sign-in. Press it again to \
                 start a new one."
                    .to_owned()
            } else {
                "the browser did not come back in time. Press it again to start a new sign-in."
                    .to_owned()
            });
        }

        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
                continue;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("the sign-in listener failed: {err}")),
        };

        // An accepted socket does not inherit the listener's non-blocking flag on
        // every platform, so it is said explicitly rather than assumed.
        if stream.set_nonblocking(false).is_err()
            || stream
                .set_read_timeout(Some(REQUEST_TIMEOUT_LOOPBACK))
                .is_err()
            || stream
                .set_write_timeout(Some(REQUEST_TIMEOUT_LOOPBACK))
                .is_err()
        {
            continue;
        }

        match request_line(&mut stream) {
            Ok(line) => match http::parse_request_line(&line) {
                Ok((method, target)) if method == "GET" && target_path(&target) == path => {
                    let query = target.split_once('?').map_or("", |(_, query)| query);
                    match http::query_pairs(query) {
                        Ok(params) if param(&params, "state") == Some(state) => {
                            return Ok(Caught { stream, params });
                        }
                        // The right door, the wrong sign-in — or a query that is
                        // not one. Answered, and the wait carries on.
                        _ => {
                            refused_a_caller = true;
                            answer(&mut stream, 400, NOT_FOUND_PAGE);
                        }
                    }
                }
                _ => answer(&mut stream, 404, NOT_FOUND_PAGE),
            },
            Err(_) => answer(&mut stream, 400, NOT_FOUND_PAGE),
        }
    }
}

/// The path half of a request target, without its query.
fn target_path(target: &str) -> &str {
    target.split_once('?').map_or(target, |(path, _)| path)
}

/// Whether the ECS has asked the thread to stop, either explicitly or by going
/// away. The mirror of `session::shutdown_requested`.
fn cancelled(commands: &Receiver<SignInCommand>) -> bool {
    match commands.try_recv() {
        Ok(SignInCommand::Cancel) | Err(TryRecvError::Disconnected) => true,
        Err(TryRecvError::Empty) => false,
    }
}

/// Reads one request line, bounded before anything is stored.
fn request_line(stream: &mut TcpStream) -> Result<String, String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err("the browser closed the connection".to_owned()),
            Ok(_) => {
                if byte[0] == b'\n' {
                    while line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return String::from_utf8(line)
                        .map_err(|_| "the browser sent a request that is not text".to_owned());
                }
                if line.len() >= MAX_REQUEST_LINE_BYTES {
                    return Err(
                        "the browser sent a request line longer than this client reads".to_owned(),
                    );
                }
                line.push(byte[0]);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(format!("the browser said nothing: {err}")),
        }
    }
}

/// Checks the redirect and spends it, up to and including the cached ticket.
///
/// **The `state` is compared before anything is sent to `finish`.** The redirect
/// carries a `code` this service will redeem exactly once, so a redirect that is
/// not the one this attempt started must not be forwarded — spending it would burn
/// somebody else's sign-in.
///
/// [`wait_for_redirect`] checks the same value one layer up, so in practice a
/// mismatch never gets this far. The check stays regardless: that one decides
/// whether to keep waiting, this one decides whether to spend a `code`, and a
/// credential is not a thing to forward on the strength of a caller having been
/// careful.
fn complete(
    service: &AccountService,
    started: &Started,
    world: Option<&str>,
    params: &[(String, String)],
    ticket_path: Option<&Path>,
    events: &Sender<SignInEvent>,
) -> Result<(), String> {
    if let Some(error) = param(params, "error") {
        // The provider's own refusal, arriving through the browser. Its vocabulary
        // is OAuth's (`access_denied`, `server_error`, …) and it is the one word
        // worth showing, because "you pressed Cancel" and "Discord is broken" are
        // different things to be told.
        return Err(format!("Discord refused the sign-in: {error}"));
    }

    let state = param(params, "state")
        .ok_or_else(|| "the redirect named no sign-in; nothing was sent on".to_owned())?;
    if state != started.state {
        return Err(
            "the redirect belongs to a different sign-in; nothing was sent on. Press it again to \
             start a new one."
                .to_owned(),
        );
    }

    let code = Secret::new(
        param(params, "code")
            .ok_or_else(|| "the redirect carried no authorization code".to_owned())?
            .to_owned(),
    );

    // **`world` is absent rather than empty when this attempt wants an account
    // ticket**, which is the same encoding `encode_client_hello` uses for a token
    // nobody holds and the one `signin.go` reads: an empty string there is "the caller
    // meant to name a world and failed", and it is refused with `world_not_named`
    // rather than quietly minting the other kind of ticket.
    let world = match world {
        Some(world) => format!(",\"world\":{}", json::quote(world)),
        None => String::new(),
    };
    let body = format!(
        "{{\"state\":{state},\"code\":{code},\"finish_secret\":{secret}{world}}}",
        state = json::quote(&started.state),
        code = json::quote(code.reveal()),
        secret = json::quote(started.finish_secret.reveal()),
    );
    let response = http::post_json(
        service.transport(),
        service.authority(),
        &service.finish_path(),
        &body,
        REQUEST_TIMEOUT,
    )?;
    let fields = readable(&response)?;

    let ticket = tickets::decode_ticket(fields.string("session_ticket")?)?;
    let expires_at = json::unix_seconds(fields.string("ticket_expires_at")?)?;
    let cached = CachedTicket::new(ticket, expires_at);

    // A sign-in that worked and could not be *saved* is still a sign-in: the player
    // is signed in for this launch, and the cost is that the next launch opens a
    // tab. Reported rather than swallowed, because a client that silently forgets
    // every launch is a bug that looks like a feature.
    match ticket_path {
        Some(path) => {
            if let Err(complaint) = tickets::write(path, cached) {
                let _ = events.send(SignInEvent::Warning(complaint));
            }
        }
        None => {
            let _ = events.send(SignInEvent::Warning(
                "no file could be named to keep this sign-in in, so the next launch will need \
                 the browser again"
                    .to_owned(),
            ));
        }
    }

    Ok(())
}

/// The first value named `name`, if the redirect carried one.
fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// A `200` with a body this client can read, or a refusal written for a player.
fn readable(response: &http::Response) -> Result<json::Fields, String> {
    if response.status == 200 {
        return json::parse_object(&response.body);
    }
    Err(refusal(response))
}

/// Turns the account service's refusal into a line a player reads.
///
/// The codes are a closed set defined in `server/cmd/voxelheim-auth/signin.go`, and
/// deliberately so: a refusal never carries a word that came from the provider or
/// from a request, which is what makes it safe to show. An unrecognised one is
/// shown as itself — it is still from that closed set, just from a newer copy of it.
fn refusal(response: &http::Response) -> String {
    let code = json::parse_object(&response.body)
        .ok()
        .and_then(|fields| fields.optional_string("error").map(str::to_owned));
    let Some(code) = code else {
        return format!(
            "the account service answered {} and nothing this client could read",
            response.status
        );
    };

    match code.as_str() {
        "sign_in_not_configured" => {
            "that account service has no Discord application configured, so it cannot sign \
             anybody in."
        }
        "sign_in_not_found" => "that sign-in has expired. Press it again to start a new one.",
        "provider_refused" => "Discord refused the sign-in.",
        "provider_unavailable" => "Discord could not be reached. Try again in a moment.",
        "too_many_sign_ins" => "the account service is busy. Try again in a moment.",
        "account_unavailable" => "the account service could not read its accounts.",
        "sign_in_could_not_start" => "the account service could not start a sign-in.",
        "ticket_unavailable" => "the account service could not issue a ticket.",
        // The one refusal that names a version rather than a fault: signing in
        // without naming a world is what an *account* ticket is, and a service that
        // predates that change refuses it. Said plainly, because "malformed" would
        // send somebody looking at this client.
        "world_not_named" => {
            "that account service does not issue account tickets yet - it will only sign for a \
             named world."
        }
        "malformed_request" => "the account service could not read this client's request.",
        other => return format!("the account service refused the sign-in: {other}"),
    }
    .to_owned()
}

/// Answers the browser and closes.
///
/// **The tab is told what actually happened**, which is why this runs after
/// `finish` rather than the moment the redirect lands: a page that said "it worked"
/// before anybody had asked would be wrong exactly when it mattered. A tab left
/// saying nothing is how a player concludes the game is broken; a tab saying the
/// wrong thing is worse.
fn answer_the_tab(stream: &mut TcpStream, worked: bool) {
    let page = if worked { DONE_PAGE } else { FAILED_PAGE };
    answer(stream, if worked { 200 } else { 400 }, page);
}

/// Writes one complete HTTP response and shuts the connection down.
///
/// Failure is ignored on purpose: the browser hanging up early costs this response
/// and nothing else, and there is no second thing to try.
fn answer(stream: &mut TcpStream, status: u16, page: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {length}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n\
         {page}",
        length = page.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// The page a finished sign-in lands on.
///
/// Self-contained: no image, no script, no font and no request to anywhere. A page
/// that fetched something would be a page that fails to render on the one machine
/// whose network is the thing being set up.
const DONE_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>Voxelheim</title><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<style>body{background:#0b0e14;color:#e6e9ef;font-family:system-ui,sans-serif;display:flex;\
align-items:center;justify-content:center;height:100vh;margin:0}main{max-width:28rem;\
text-align:center;padding:2rem}h1{font-size:1.5rem;margin:0 0 .75rem}p{margin:0;color:#9aa3b2}\
</style></head><body><main><h1>You are signed in.</h1>\
<p>You can close this tab and go back to the game.</p></main></body></html>";

/// The page a sign-in that did not finish lands on.
const FAILED_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>Voxelheim</title><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<style>body{background:#0b0e14;color:#e6e9ef;font-family:system-ui,sans-serif;display:flex;\
align-items:center;justify-content:center;height:100vh;margin:0}main{max-width:28rem;\
text-align:center;padding:2rem}h1{font-size:1.5rem;margin:0 0 .75rem}p{margin:0;color:#9aa3b2}\
</style></head><body><main><h1>That sign-in did not finish.</h1>\
<p>You can close this tab. The game says why, and can start another one.</p></main></body></html>";

/// What anything else that finds the loopback port is told.
const NOT_FOUND_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>Voxelheim</title></head><body></body></html>";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::codec::SESSION_TICKET_LEN;
    use crate::net::session::Scratch;
    use std::sync::mpsc;

    /// A ticket, as the account service would encode one.
    fn encoded_ticket(byte: u8) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let bytes = [byte; SESSION_TICKET_LEN];
        let mut out = String::new();
        for group in bytes.chunks(3) {
            let mut accumulator = 0u32;
            for (index, value) in group.iter().enumerate() {
                accumulator |= u32::from(*value) << (16 - 8 * index);
            }
            for index in 0..group.len() + 1 {
                let sextet = usize::try_from((accumulator >> (18 - 6 * index)) & 0x3F)
                    .expect("six bits is an index");
                out.push(char::from(ALPHABET[sextet]));
            }
        }
        out
    }

    /// What the fake service answers `finish` with.
    #[derive(Clone)]
    enum FinishAnswer {
        Ticket { encoded: String, expires_at: String },
        Refusal { status: u16, code: String },
    }

    /// A stand-in for `voxelheim-auth`, on a real loopback socket.
    ///
    /// It answers exactly the two routes this client calls and records what it was
    /// asked, which is what lets a test assert that `finish_secret` reached the
    /// body — and only the body.
    struct FakeService {
        authority: String,
        requests: Receiver<(String, String)>,
    }

    impl FakeService {
        fn spawn(redirect: &str, finish: FinishAnswer, state: &str, secret: &str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let authority = listener.local_addr().expect("an address").to_string();
            let (sender, requests) = mpsc::channel();
            let redirect = redirect.to_owned();
            let state = state.to_owned();
            let secret = secret.to_owned();

            thread::spawn(move || {
                for _ in 0..2 {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    let mut raw = Vec::new();
                    let mut chunk = [0u8; 1024];
                    // The client says `Connection: close` and sends everything at
                    // once, so one read is the whole request in practice; the loop
                    // is what makes that not a requirement.
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(read) => {
                                raw.extend_from_slice(&chunk[..read]);
                                let text = String::from_utf8_lossy(&raw).into_owned();
                                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                                    let length: usize = head
                                        .lines()
                                        .find_map(|line| {
                                            line.strip_prefix("Content-Length: ")
                                                .and_then(|value| value.trim().parse().ok())
                                        })
                                        .unwrap_or(0);
                                    if body.len() >= length {
                                        break;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let text = String::from_utf8_lossy(&raw).into_owned();
                    let path = text.split(' ').nth(1).unwrap_or_default().to_owned();
                    let body = text
                        .split_once("\r\n\r\n")
                        .map(|(_, body)| body.to_owned())
                        .unwrap_or_default();
                    let _ = sender.send((path.clone(), body));

                    let (status, payload) = if path.ends_with(START_PATH) {
                        (
                            200,
                            format!(
                                "{{\"state\":\"{state}\",\"finish_secret\":\"{secret}\",\
                                 \"authorize_url\":\"https://discord.invalid/oauth2/authorize?\
                                 client_id=1\\u0026state={state}\\u0026redirect_uri={redirect}\",\
                                 \"expires_at\":\"2099-01-01T00:00:00Z\"}}"
                            ),
                        )
                    } else {
                        match &finish {
                            FinishAnswer::Ticket {
                                encoded,
                                expires_at,
                            } => (
                                200,
                                format!(
                                    "{{\"account_id\":\"0f\",\"display_name\":\"someone\",\
                                     \"created\":true,\"session_ticket\":\"{encoded}\",\
                                     \"ticket_expires_at\":\"{expires_at}\"}}"
                                ),
                            ),
                            FinishAnswer::Refusal { status, code } => {
                                (*status, format!("{{\"error\":\"{code}\"}}"))
                            }
                        }
                    };

                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {len}\r\nConnection: close\r\n\r\n{payload}",
                        len = payload.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            });

            Self {
                authority,
                requests,
            }
        }

        fn service(&self) -> AccountService {
            AccountService::plaintext(&format!("http://{}", self.authority)).expect("a URL")
        }

        fn seen(&self) -> Vec<(String, String)> {
            self.requests.try_iter().collect()
        }
    }

    /// Plays the browser: takes the authorize URL, follows its redirect target the
    /// way a provider would, and answers with `query`.
    fn follow_redirect(url: &str, query: &str) -> String {
        let authorize = http::parse_url(url).expect("an authorize URL");
        let redirect = http::query_pairs(&authorize.query)
            .expect("a query")
            .into_iter()
            .find(|(name, _)| name == "redirect_uri")
            .map(|(_, value)| value)
            .expect("a redirect");
        let redirect = http::parse_url(&redirect).expect("a redirect URL");

        let mut stream = TcpStream::connect(redirect.authority()).expect("the loopback listener");
        let request = format!(
            "GET {}?{query} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            redirect.path,
            redirect.authority()
        );
        stream.write_all(request.as_bytes()).expect("a request");
        stream.flush().expect("a flush");
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    }

    /// A loopback port for a redirect URI to name — **and the listener that holds
    /// it**, which is the whole point of the pair.
    ///
    /// The listener is returned rather than dropped because dropping it is #557:
    /// between the drop and the moment the attempt binds, the port belongs to
    /// nobody, and every other test in this binary that asks for an ephemeral port
    /// — nine more sign-ins, `FakeService::spawn`, `net/mod.rs`, `net/tls.rs` — is
    /// asking the kernel for exactly the number just released. Hand this listener
    /// to [`Loopback::Prebound`] and the port passes from the test to the attempt
    /// without ever being free.
    fn reserved_loopback_port() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        (listener, port)
    }

    /// **The production path, which every driven attempt now steps around.** Ten
    /// tests below hand `run` a listener they bound themselves, so without this one
    /// nothing would exercise [`bind`] at all.
    ///
    /// The collision is the assertion: a second listener on an address another
    /// socket is actively listening on is refused by the kernel (`SO_REUSEADDR`,
    /// which Rust sets, relaxes `TIME_WAIT` and not this), so the refusal is proof
    /// `bind` aimed at exactly the port the redirect named and at no other. Written
    /// as a collision rather than as a success because the success direction would
    /// need a port known to be free, and learning one by releasing it is the bug
    /// this whole change is about.
    #[test]
    fn bind_refuses_the_redirects_port_when_something_already_holds_it() {
        let (held, port) = reserved_loopback_port();
        let redirect = http::parse_url(&format!("http://127.0.0.1:{port}/discord/callback"))
            .expect("a redirect URL");

        let refusal = bind(&redirect).expect_err("the port is held by this test");
        assert!(refusal.contains(&format!("127.0.0.1:{port}")), "{refusal}");
        assert!(refusal.contains("may already be signing in"), "{refusal}");

        drop(held);
    }

    struct Attempt {
        events: Vec<SignInEvent>,
        browser_url: String,
        requests: Vec<(String, String)>,
    }

    /// Drives one whole attempt against the fake service, playing the browser.
    ///
    /// `listener` is the redirect port's own, still bound — see
    /// [`reserved_loopback_port`].
    fn drive(
        service: &FakeService,
        listener: TcpListener,
        ticket_path: Option<PathBuf>,
        query: impl Fn(&str) -> String,
    ) -> Attempt {
        drive_for_world(service, listener, None, ticket_path, query)
    }

    /// The same, asking for a ticket scoped to `world` when there is one.
    fn drive_for_world(
        service: &FakeService,
        listener: TcpListener,
        world: Option<&str>,
        ticket_path: Option<PathBuf>,
        query: impl Fn(&str) -> String,
    ) -> Attempt {
        let (event_tx, event_rx) = mpsc::channel();
        let (browser_tx, browser_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let account = service.service();
        let world = world.map(str::to_owned);

        let worker = thread::spawn(move || {
            run(
                account,
                world,
                ticket_path,
                Loopback::Prebound(listener),
                Browser::Captured(browser_tx),
                event_tx,
                command_rx,
            );
        });

        let browser_url = browser_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the client opens a browser");
        let query = query(&browser_url);
        if !query.is_empty() {
            follow_redirect(&browser_url, &query);
        }

        let events: Vec<SignInEvent> = event_rx.iter().collect();
        drop(command_tx);
        worker.join().expect("the sign-in thread");

        Attempt {
            events,
            browser_url,
            requests: service.seen(),
        }
    }

    fn state_from(url: &str) -> String {
        http::query_pairs(&http::parse_url(url).expect("a URL").query)
            .expect("a query")
            .into_iter()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value)
            .expect("a state")
    }

    #[test]
    fn a_whole_sign_in_ends_with_a_ticket_on_disk() {
        let (listener, port) = reserved_loopback_port();
        let redirect = format!("http://127.0.0.1:{port}/discord/callback");
        let service = FakeService::spawn(
            &redirect,
            FinishAnswer::Ticket {
                encoded: encoded_ticket(0x5a),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "the-state",
            "the-finish-secret",
        );
        let scratch = Scratch::new("signin-happy");
        let path = scratch.join("service");

        let attempt = drive(&service, listener, Some(path.clone()), |url| {
            format!("code=the-code&state={}", state_from(url))
        });

        assert_eq!(attempt.events, vec![SignInEvent::Completed]);
        let (cached, complaint) = tickets::read(&path);
        assert_eq!(complaint, None);
        let cached = cached.expect("a cached ticket");
        assert!(cached.is_live(0), "a ticket that expires in 2099");
        assert_eq!(
            cached.ticket(),
            crate::net::codec::SessionTicket::from_bytes([0x5a; SESSION_TICKET_LEN])
        );
    }

    #[test]
    fn the_finish_body_names_no_world_at_all() {
        // An *account* ticket is what a sign-in produces before a world has been
        // chosen, and the contract's encoding of "no world" is an absent field —
        // the same choice `encode_client_hello` makes for an absent token.
        let (listener, port) = reserved_loopback_port();
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Ticket {
                encoded: encoded_ticket(1),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "s",
            "sec",
        );
        let scratch = Scratch::new("signin-world");
        let attempt = drive(&service, listener, Some(scratch.join("service")), |url| {
            format!("code=c&state={}", state_from(url))
        });

        let finish = attempt
            .requests
            .iter()
            .find(|(path, _)| path.ends_with(FINISH_PATH))
            .expect("a finish request");
        assert!(!finish.1.contains("world"), "{}", finish.1);
        assert!(finish.1.contains("finish_secret"), "{}", finish.1);
    }

    /// **The other half of the same encoding, and what #154 needed.** A game server
    /// admits nobody on an account ticket, so a launch that names an address has to
    /// name the world too — and that name reaches `finish`, which is the only place a
    /// world-scoped ticket is minted.
    #[test]
    fn a_sign_in_for_a_world_names_it_in_the_finish_body() {
        let (listener, port) = reserved_loopback_port();
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Ticket {
                encoded: encoded_ticket(2),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "s",
            "sec",
        );
        let scratch = Scratch::new("signin-for-world");
        let attempt = drive_for_world(
            &service,
            listener,
            Some("midgard"),
            Some(scratch.join("service")),
            |url| format!("code=c&state={}", state_from(url)),
        );

        assert_eq!(attempt.events, vec![SignInEvent::Completed]);
        let finish = attempt
            .requests
            .iter()
            .find(|(path, _)| path.ends_with(FINISH_PATH))
            .expect("a finish request");
        assert!(finish.1.contains("\"world\":\"midgard\""), "{}", finish.1);
    }

    #[test]
    fn nothing_a_sign_in_produces_carries_the_code_the_secret_or_the_ticket() {
        // The criterion this test exists for: a whole sign-in is captured and each
        // of the three credentials is looked for in raw, hex and base64 form.
        let (listener, port) = reserved_loopback_port();
        let code = "authcodeAUTHCODE";
        let secret = "finishsecretFINISH";
        let ticket = encoded_ticket(0x7e);
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Ticket {
                encoded: ticket.clone(),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "the-state",
            secret,
        );
        let scratch = Scratch::new("signin-quiet");
        let attempt = drive(&service, listener, Some(scratch.join("service")), |url| {
            format!("code={code}&state={}", state_from(url))
        });

        // Everything this attempt said to anyone but the account service: the
        // events, their `Debug`, and the URL handed to the browser.
        let mut spoken = format!("{:?}", attempt.events);
        for event in &attempt.events {
            match event {
                SignInEvent::Refused(text) | SignInEvent::Warning(text) => {
                    spoken.push_str(text);
                }
                SignInEvent::Completed => {}
            }
        }
        spoken.push_str(&attempt.browser_url);
        spoken.push_str(&format!("{:?}", Secret::new(secret.to_owned())));

        for (what, value) in [
            ("code", code),
            ("finish secret", secret),
            ("ticket", &ticket),
        ] {
            for (shape, spelling) in [
                ("raw", value.to_owned()),
                ("hex", hex(value.as_bytes())),
                ("base64", base64(value.as_bytes())),
            ] {
                assert!(
                    !spoken.contains(&spelling),
                    "the {what} appears in {shape} in what this sign-in said: {spoken}"
                );
            }
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for group in bytes.chunks(3) {
            let mut accumulator = 0u32;
            for (index, value) in group.iter().enumerate() {
                accumulator |= u32::from(*value) << (16 - 8 * index);
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
    fn nothing_without_this_sign_ins_state_ends_the_wait_or_is_sent_on() {
        // The redirect port and path are registered configuration, so any page on
        // this machine can knock. None of these may end the wait, and the real
        // redirect arriving afterwards must still work.
        let (listener, port) = reserved_loopback_port();
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Ticket {
                encoded: encoded_ticket(2),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "the-state",
            "sec",
        );
        let scratch = Scratch::new("signin-state");
        let path = scratch.join("service");

        let (event_tx, event_rx) = mpsc::channel();
        let (browser_tx, browser_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let account = service.service();
        let ticket_path = path.clone();
        let worker = thread::spawn(move || {
            run(
                account,
                None,
                Some(ticket_path),
                Loopback::Prebound(listener),
                Browser::Captured(browser_tx),
                event_tx,
                command_rx,
            );
        });

        let url = browser_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a browser");

        for query in [
            // Somebody else's sign-in, carrying a code that must not be spent.
            "code=the-code&state=somebody-elses",
            // The abort an `<img src>` would be: no state at all.
            "error=access_denied",
            // A query that does not decode. This used to end the whole attempt.
            "code=%zz&state=the-state",
        ] {
            let answer = follow_redirect(&url, query);
            assert!(answer.contains("400"), "{query} -> {answer}");
            assert!(!answer.contains("You are signed in"), "{query} -> {answer}");
        }

        let page = follow_redirect(&url, &format!("code=c&state={}", state_from(&url)));
        assert!(page.contains("200 OK"), "{page}");
        assert!(page.contains("You are signed in"), "{page}");

        let events: Vec<SignInEvent> = event_rx.iter().collect();
        assert_eq!(events, vec![SignInEvent::Completed]);
        drop(command_tx);
        worker.join().expect("the sign-in thread");

        let requests = service.seen();
        let finishes: Vec<&(String, String)> = requests
            .iter()
            .filter(|(path, _)| path.ends_with(FINISH_PATH))
            .collect();
        assert_eq!(finishes.len(), 1, "{finishes:?}");
        assert!(
            finishes[0].1.contains("\"code\":\"c\""),
            "only the real redirect's code may be spent"
        );
        assert!(tickets::read(&path).0.is_some());
    }

    #[test]
    fn complete_refuses_a_state_it_did_not_start_before_it_sends_anything() {
        // `wait_for_redirect` now filters on `state` too, so this is the second of
        // two gates rather than the only one — and it is the one that guards the
        // request, so it is checked on its own rather than through an attempt.
        let (events, _drain) = mpsc::channel();
        // Discard: reaching a socket at all would be the failure under test.
        let service = AccountService::plaintext("http://127.0.0.1:9").expect("a URL");
        let started = Started {
            state: "the-state".to_owned(),
            finish_secret: Secret::new("sec".to_owned()),
            authorize_url: String::new(),
            deadline: Instant::now() + Duration::from_secs(60),
        };
        let params = vec![
            ("state".to_owned(), "somebody-elses".to_owned()),
            ("code".to_owned(), "the-code".to_owned()),
        ];
        let refusal =
            complete(&service, &started, None, &params, None, &events).expect_err("a refusal");
        assert!(refusal.contains("different sign-in"), "{refusal}");
    }

    #[test]
    fn a_provider_refusal_arriving_through_the_browser_is_reported_as_one() {
        let (listener, port) = reserved_loopback_port();
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Ticket {
                encoded: encoded_ticket(3),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "s",
            "sec",
        );
        let scratch = Scratch::new("signin-denied");
        // A real refusal echoes the `state` (RFC 6749 §4.1.2.1), which is what
        // distinguishes "the player pressed Cancel" from a page on this machine
        // shouting `error=access_denied` at the loopback port.
        let attempt = drive(&service, listener, Some(scratch.join("service")), |url| {
            format!("error=access_denied&state={}", state_from(url))
        });

        match attempt.events.as_slice() {
            [SignInEvent::Refused(reason)] => assert!(reason.contains("access_denied"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_service_that_will_not_sign_for_an_unnamed_world_says_so_in_words() {
        let (listener, port) = reserved_loopback_port();
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Refusal {
                status: 400,
                code: "world_not_named".to_owned(),
            },
            "s",
            "sec",
        );
        let scratch = Scratch::new("signin-noworld");
        let path = scratch.join("service");
        let attempt = drive(&service, listener, Some(path.clone()), |url| {
            format!("code=c&state={}", state_from(url))
        });

        match attempt.events.as_slice() {
            [SignInEvent::Refused(reason)] => {
                assert!(reason.contains("account ticket"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(tickets::read(&path).0, None);
    }

    #[test]
    fn the_browser_tab_is_told_what_happened() {
        let (listener, port) = reserved_loopback_port();
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Ticket {
                encoded: encoded_ticket(4),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "the-state",
            "sec",
        );
        let scratch = Scratch::new("signin-tab");

        let (event_tx, event_rx) = mpsc::channel();
        let (browser_tx, browser_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let account = service.service();
        let path = scratch.join("service");
        let worker = thread::spawn(move || {
            run(
                account,
                None,
                Some(path),
                Loopback::Prebound(listener),
                Browser::Captured(browser_tx),
                event_tx,
                command_rx,
            );
        });

        let url = browser_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a browser");
        let page = follow_redirect(&url, &format!("code=c&state={}", state_from(&url)));
        assert!(page.contains("200 OK"), "{page}");
        assert!(page.contains("You are signed in"), "{page}");
        assert!(page.contains("close this tab"), "{page}");

        let events: Vec<SignInEvent> = event_rx.iter().collect();
        assert_eq!(events, vec![SignInEvent::Completed]);
        drop(command_tx);
        worker.join().expect("the sign-in thread");
    }

    #[test]
    fn the_listener_answers_only_the_redirect_and_stops_after_it() {
        let (listener, port) = reserved_loopback_port();
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Ticket {
                encoded: encoded_ticket(5),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "the-state",
            "sec",
        );
        let scratch = Scratch::new("signin-listener");
        let (event_tx, event_rx) = mpsc::channel();
        let (browser_tx, browser_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let account = service.service();
        let path = scratch.join("service");
        let worker = thread::spawn(move || {
            run(
                account,
                None,
                Some(path),
                Loopback::Prebound(listener),
                Browser::Captured(browser_tx),
                event_tx,
                command_rx,
            );
        });

        let url = browser_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a browser");

        // A browser opens more than one connection. Something that is not the
        // redirect is answered and does not end the wait.
        let mut stray = TcpStream::connect(format!("127.0.0.1:{port}")).expect("the listener");
        stray
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .expect("a request");
        let mut answer = String::new();
        let _ = stray.read_to_string(&mut answer);
        assert!(answer.contains("404"), "{answer}");

        let page = follow_redirect(&url, &format!("code=c&state={}", state_from(&url)));
        assert!(page.contains("200 OK"), "{page}");

        let events: Vec<SignInEvent> = event_rx.iter().collect();
        assert_eq!(events, vec![SignInEvent::Completed]);
        drop(command_tx);
        worker.join().expect("the sign-in thread");

        // And the port is free again, which is what "stops" means.
        assert!(
            TcpListener::bind(format!("127.0.0.1:{port}")).is_ok(),
            "the listener outlived the sign-in"
        );
    }

    #[test]
    fn a_sign_in_that_cannot_be_saved_still_says_it_happened() {
        let (listener, port) = reserved_loopback_port();
        let service = FakeService::spawn(
            &format!("http://127.0.0.1:{port}/discord/callback"),
            FinishAnswer::Ticket {
                encoded: encoded_ticket(6),
                expires_at: "2099-01-01T08:00:00Z".to_owned(),
            },
            "s",
            "sec",
        );
        let attempt = drive(&service, listener, None, |url| {
            format!("code=c&state={}", state_from(url))
        });

        assert!(
            matches!(
                attempt.events.as_slice(),
                [SignInEvent::Warning(_), SignInEvent::Completed]
            ),
            "{:?}",
            attempt.events
        );
    }

    #[test]
    fn an_account_service_url_is_read_or_refused_with_a_reason() {
        let pin = "ab".repeat(32);

        let service = AccountService::parse("https://127.0.0.1:7780", &pin).expect("a URL");
        assert_eq!(service.authority(), "127.0.0.1:7780");
        assert_eq!(service.start_path(), START_PATH);
        assert_eq!(service.finish_path(), FINISH_PATH);
        assert_eq!(service.to_string(), "https://127.0.0.1:7780");

        // No port, so the scheme decides it — 443 rather than the 80 a single default
        // would have dialled at an address nobody typed wrong.
        let prefixed =
            AccountService::parse("https://accounts.example/auth/", &pin).expect("a URL");
        assert_eq!(prefixed.authority(), "accounts.example:443");
        assert_eq!(prefixed.start_path(), format!("/auth{START_PATH}"));

        // **The plaintext refusal, which is the direction that matters** (#131): there is
        // no unencrypted account service to reach, so `http` is a usage error before a
        // window opens rather than a connection failure on a login screen.
        let err = AccountService::parse("http://accounts.example", &pin).expect_err("no plaintext");
        assert!(err.contains("listens over TLS"), "{err}");
        assert!(AccountService::parse("accounts.example", &pin).is_err());
        assert!(AccountService::parse("ftp://accounts.example", &pin).is_err());
        assert!(AccountService::parse("https://accounts.example/?a=1", &pin).is_err());

        // And a fingerprint that is not a SHA-256 is refused whatever the address is. A
        // service reached with nothing to compare against is the hole, so there is no
        // shape of this type that carries a URL and no expectation.
        for bad in [
            "",
            "not a fingerprint",
            &"ab".repeat(31),
            &"ab".repeat(33),
            &"zz".repeat(32),
        ] {
            assert!(
                AccountService::parse("https://accounts.example", bad).is_err(),
                "{bad} was accepted as a certificate fingerprint"
            );
        }

        // Case is folded rather than refused, which is `tls::parse_fingerprint`'s call:
        // the value is compared against a digest this client computes, and an operator
        // pasting an uppercase copy of their own log line has not made a mistake.
        let upper = AccountService::parse("https://accounts.example", &"AB".repeat(32))
            .expect("an uppercase digest");
        assert_eq!(upper.transport(), &http::Transport::Pinned("ab".repeat(32)));
    }

    #[test]
    fn a_redirect_that_is_not_loopback_is_refused_before_anything_binds() {
        let public =
            "https://discord.invalid/authorize?redirect_uri=http%3A%2F%2F203.0.113.5%3A80%2Fcb";
        let err = loopback_redirect(public).expect_err("not loopback");
        assert!(err.contains("loopback"), "{err}");

        let https =
            "https://discord.invalid/authorize?redirect_uri=https%3A%2F%2F127.0.0.1%3A80%2Fcb";
        assert!(loopback_redirect(https).is_err());

        let none = "https://discord.invalid/authorize?client_id=1";
        assert!(loopback_redirect(none).is_err());

        for host in ["127.0.0.1", "127.9.9.9", "::1", "localhost", "LOCALHOST"] {
            assert!(is_loopback(host), "{host}");
        }
        for host in ["203.0.113.5", "example.invalid", "0.0.0.0", "::"] {
            assert!(!is_loopback(host), "{host}");
        }
    }

    #[test]
    fn a_redirect_naming_port_zero_is_refused_rather_than_bound() {
        // It would bind, and that is the trap: the browser is sent to the literal
        // URL, so whatever the kernel picked is a port nobody was told about.
        let zero = "https://discord.invalid/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A0%2Fcb";
        let err = loopback_redirect(zero).expect_err("port 0");
        assert!(err.contains("port 0"), "{err}");
    }

    #[test]
    fn an_ipv6_loopback_redirect_gets_an_address_a_socket_can_use() {
        // `::1` is loopback and accepted, so the authority it turns into has to be
        // one `TcpListener::bind` and `TcpStream::connect` accept: `::1:7780` is
        // not, and `bind` builds its address from exactly this.
        let ipv6 =
            "https://discord.invalid/authorize?redirect_uri=http%3A%2F%2F%5B%3A%3A1%5D%3A7780%2Fcb";
        let redirect = loopback_redirect(ipv6).expect("a loopback redirect");
        assert_eq!(redirect.host, "::1");
        assert_eq!(redirect.authority(), "[::1]:7780");
    }

    #[test]
    fn the_browser_wait_is_clamped_at_both_ends() {
        assert_eq!(browser_wait(None), MAX_BROWSER_WAIT);
        assert_eq!(browser_wait(Some("not a time")), MAX_BROWSER_WAIT);
        // Long past: a clock that is out must not mean a wait of zero.
        assert_eq!(browser_wait(Some("1971-01-01T00:00:00Z")), MIN_BROWSER_WAIT);
        // Far future: bounded by what this client is willing to hold a port for.
        assert_eq!(browser_wait(Some("2099-01-01T00:00:00Z")), MAX_BROWSER_WAIT);
    }

    #[test]
    fn a_refusal_is_prose_rather_than_a_code_wherever_the_service_has_one() {
        for (code, wanted) in [
            ("sign_in_not_found", "expired"),
            ("provider_refused", "Discord"),
            ("too_many_sign_ins", "busy"),
            ("world_not_named", "account ticket"),
            ("sign_in_not_configured", "Discord application"),
        ] {
            let text = refusal(&http::Response {
                status: 400,
                body: format!("{{\"error\":\"{code}\"}}"),
            });
            assert!(text.contains(wanted), "{code} -> {text}");
        }

        // An unrecognised code is still from a closed set, so it is shown as itself.
        let text = refusal(&http::Response {
            status: 400,
            body: "{\"error\":\"something_new\"}".to_owned(),
        });
        assert!(text.contains("something_new"), "{text}");

        // A body that says nothing readable still says the status.
        let text = refusal(&http::Response {
            status: 502,
            body: "<html>".to_owned(),
        });
        assert!(text.contains("502"), "{text}");
    }

    #[test]
    fn a_secret_prints_nothing_of_itself() {
        let secret = Secret::new("sup3rsecret".to_owned());
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert_eq!(secret.reveal(), "sup3rsecret");
    }

    #[test]
    fn a_target_is_split_from_its_query() {
        assert_eq!(target_path("/discord/callback?code=a"), "/discord/callback");
        assert_eq!(target_path("/discord/callback"), "/discord/callback");
    }
}
