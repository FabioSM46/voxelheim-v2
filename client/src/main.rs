//! Voxelheim client — the rendering half of an authoritative session.
//!
//! This binary opens a window, connects to a `voxelheimd`, completes the
//! handshake and says on screen how that went. It decides nothing: the server
//! owns every gameplay outcome, and the client's job is to transport and display
//! them. See `client/AGENTS.md` for the rules that shape the code below.

// The generated FlatBuffers bindings.
//
// Declared under a different Rust name than its directory for one boring reason:
// `gen` is a reserved keyword in edition 2024, so `mod gen;` does not parse. The
// directory keeps the repository-wide `gen/` name that the review bot and
// AGENTS.md's no-hand-editing rule both key on.
//
// The allow list is the narrowest set that silences flatc's output, and each
// entry is there because flatc emits it:
//   * unused_imports        — every generated file opens with `use super::*`
//   * dead_code             — builders and accessors for messages this issue does
//                             not send or read yet, plus flatc's ENUM_* constants
//   * extra_unused_lifetimes— `impl<'a> Follow<'a>` on the zero-field offset enums
//   * derivable_impls       — hand-written `Default` impls flatc could derive
// Anything beyond that list is a signal to look at the generator, not to widen it.
#[path = "gen/mod.rs"]
#[allow(
    unused_imports,
    dead_code,
    clippy::extra_unused_lifetimes,
    clippy::derivable_impls
)]
mod wire;

mod net;
mod player;
mod settings;
mod ui;
mod world;

use std::path::PathBuf;
use std::process::ExitCode;

use bevy::prelude::*;

use crate::net::{AccountService, DEFAULT_PLAYER_NAME, NetPlugin, ServerListPlugin, SignInPlugin};
use crate::player::PlayerPlugin;
use crate::settings::SettingsPlugin;
use crate::ui::{PlayAs, UiPlugin};
use crate::world::WorldPlugin;

/// Where the client connects when neither an address nor an account service says
/// otherwise — the address `voxelheimd` listens on by default
/// (`server/cmd/voxelheimd/main.go`).
///
/// **It is the development default, not a player's.** A client with an account service
/// takes every address out of the server list; this is what a client with no list at
/// all falls back to, which is a developer with a `voxelheimd` running beside them.
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:7777";

/// Appended when the address names a host with no port.
const DEFAULT_SERVER_PORT: u16 = 7777;

/// Environment variable holding the server address. Lower precedence than the
/// command line, so a shell export is a default rather than an override.
const SERVER_ADDR_ENV: &str = "VOXELHEIM_SERVER";

/// Environment variable holding the display name, with the same precedence.
const PLAYER_NAME_ENV: &str = "VOXELHEIM_NAME";

/// Environment variable holding the identity file, with the same precedence.
///
/// It names one file, not a directory: it replaces the per-server derivation
/// outright, which is how one machine runs two characters against one server.
const IDENTITY_ENV: &str = "VOXELHEIM_IDENTITY";

/// Environment variable holding the world to ask a ticket for, with the same
/// precedence.
const WORLD_ENV: &str = "VOXELHEIM_WORLD";

/// Environment variable holding the account service URL, with the same precedence.
///
/// There is deliberately **no default**. An account service is something an
/// operator runs, and pointing at one that is not there would put a login screen in
/// front of every launch with nothing behind it — so signing in is opt-in, and a
/// client given no service behaves exactly as it did before signing in existed.
const ACCOUNT_SERVICE_ENV: &str = "VOXELHEIM_ACCOUNT_SERVICE";

/// Environment variable holding the account service's certificate fingerprint, with the
/// same precedence.
///
/// **It travels the way the address does, and that is the design rather than a
/// convenience** (#131). The account service is where this client's trust begins, so
/// there is nothing above it to learn the number from — no list, no certificate
/// authority, no first connection to remember. Whoever runs the service reads it out of
/// their own startup line and hands it over with the address, once.
const ACCOUNT_SERVICE_FINGERPRINT_ENV: &str = "VOXELHEIM_ACCOUNT_SERVICE_FINGERPRINT";

const USAGE: &str = "\
Voxelheim client

Usage:
  voxelheim-client --account-service URL --account-service-fingerprint SHA256
  voxelheim-client --account-service URL --account-service-fingerprint SHA256 \
                   --server ADDRESS --world NAME
  voxelheim-client [ADDRESS]
  voxelheim-client --help

Arguments:
  ADDRESS         host:port of a server to develop against. A bare host gets
                  port 7777.

Options:
  -a, --account-service
                  https URL of the account service to sign in against. The
                  servers you can join come from its list; you never type an
                  address.
      --account-service-fingerprint
                  the SHA-256 of the certificate that service presents, as it
                  prints it at startup (certificate_sha256). Required with
                  --account-service: this client checks it instead of a
                  certificate authority, and there is no way to skip the check.
  -s, --server    the same address as ADDRESS, named explicitly
  -w, --world     which world to ask for a ticket for, with --server. Only with
                  --server: from the list, the row you click names the world.
  -n, --name      the character to play, created if this account has none
                  wearing it. Without it the character screen asks.
  -i, --identity  file holding this server's identity token
  -h, --help      print this and exit

Environment:
  VOXELHEIM_ACCOUNT_SERVICE  used when --account-service is not given
  VOXELHEIM_ACCOUNT_SERVICE_FINGERPRINT
                             used when --account-service-fingerprint is not given
  VOXELHEIM_SERVER           used when no address is given on the command line
  VOXELHEIM_WORLD            used when --world is not given
  VOXELHEIM_NAME             used when --name is not given
  VOXELHEIM_IDENTITY         used when --identity is not given

There are three ways to launch this and they are not interchangeable.

  --account-service and its fingerprint alone is the path a player takes. You
  sign in with Discord once and then pick a server out of the list it answers
  with. The connection to the service is encrypted and pinned to the fingerprint
  you were given, so the authorization code, the sign-in secret and the ticket
  that comes back are unreadable on the way. The list then carries each server's
  address and the fingerprint of the certificate it presents, so the address is
  followed if it moves and a server presenting anything else is refused before
  this client sends a byte. There is no way past either refusal, by design: ask
  whoever runs the server to register the fingerprint it logs at startup.

  --account-service with --server and --world is the development path. You sign
  in the same way, but the address is the one you typed and the world is the one
  you named, because an address in no list comes with neither. Nothing states
  which certificate to expect there, so the session is encrypted and UNVERIFIED
  -- it is a server you chose to trust by typing its address. The ticket that
  sign-in produces is presented to it, and that is a deliberate trade: a ticket
  names one world, expires in hours, and is refused at every other world, so
  what that address can do with it is bounded and short.

  An ADDRESS or --server on its own presents no account at all. A server admits
  players on a signed ticket, so it will refuse this and say so. It stays
  because it is the honest answer to a launch that named nowhere to sign in.

The name defaults to voxelheim; with neither an account service nor an address,
the address defaults to 127.0.0.1:7777.

Who goes in is chosen after the server answers, on a screen that lists this
account's characters on that world and offers to make another when there is
room. --name skips that screen: it asks for the character wearing that name and
has one created under it when this account holds none, which is what the server
used to do with the name a hello carried. The server decides either way -- a
name it refuses is refused with --name too.

Sign-ins are kept under $XDG_DATA_HOME/voxelheim, falling back to
$HOME/.local/share: account/<service> for the ticket the server list is read
with, and world-ticket/<service>/<world> for one you join a world with. They are
separate files because the two are not interchangeable. Deleting one is how you
sign out of it.

Identity is remembered per server: without --identity the token a server issues
is kept in $XDG_DATA_HOME/voxelheim/identity/<address>. Coming back with it
resumes the same character; a token is only ever meaningful to the server that
issued it, and is only ever presented to one whose certificate the list named.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let environment = LaunchEnv {
        server_addr: std::env::var(SERVER_ADDR_ENV).ok(),
        player_name: std::env::var(PLAYER_NAME_ENV).ok(),
        identity_path: std::env::var(IDENTITY_ENV).ok(),
        account_service: std::env::var(ACCOUNT_SERVICE_ENV).ok(),
        account_service_fingerprint: std::env::var(ACCOUNT_SERVICE_FINGERPRINT_ENV).ok(),
        world: std::env::var(WORLD_ENV).ok(),
    };

    match parse_launch(&args, &environment) {
        Ok(Launch::Usage) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Launch::Connect(start)) => {
            if run(start).is_success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(problem) => {
            eprintln!("voxelheim-client: {problem}\n\n{USAGE}");
            // 2 for a usage error, as the server's flag handling does.
            ExitCode::from(2)
        }
    }
}

fn run(start: Start) -> AppExit {
    let Start {
        server_addr,
        player_name,
        chosen_character,
        identity_path,
        account_service,
        world,
    } = start;

    // The address is not in the title on the list path, and cannot be: nothing has
    // been chosen when the window opens. It is on the development path, where it was
    // typed on the command line and is the one thing distinguishing two windows.
    let title = match &server_addr {
        Some(addr) => format!("Voxelheim - {addr}"),
        None => "Voxelheim".to_owned(),
    };

    // The three shapes `parse_launch` admits, and nothing else can be built here.
    // `listening` waits for a row of the list to be clicked; `developing_against` dials
    // at build with no account to present; `developing_against_signed_in` waits for the
    // sign-in and then dials the address that was typed, presenting the ticket it
    // produced.
    let net = match (server_addr, &account_service) {
        (Some(addr), Some(_)) => NetPlugin::developing_against_signed_in(addr),
        (Some(addr), None) => NetPlugin::developing_against(addr),
        (None, _) => NetPlugin::listening(),
    };

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window { title, ..default() }),
        ..default()
    }))
    .add_plugins(
        net.with_player_name(player_name)
            .with_identity_path(identity_path),
    )
    .add_plugins(WorldPlugin)
    // Before the two that read it: `player` takes the mouse sensitivity and the key
    // bindings out of the resource this inserts, and `ui` draws the screen that writes
    // them. It is the whole of the file this client keeps for a player's preferences —
    // nothing here reaches the wire, and nothing here decides an outcome.
    .add_plugins(SettingsPlugin::from_environment())
    // The player before the UI: the player plugin owns the one camera, and `bevy_ui`
    // draws through it. See the module comment in player/camera.rs.
    .add_plugins(PlayerPlugin)
    .add_plugins(UiPlugin);

    // After the plugin, which initialises the launch-named-nobody default: a launch that
    // named somebody replaces it, and the character screen answers itself. See
    // `ui::PlayAs` for why `--name` is what says who.
    if let Some(character) = chosen_character {
        app.insert_resource(PlayAs::named(character));
    }

    // Built only when there is a service to sign in against, which is what makes the
    // login screen absent rather than broken on a client that has none. See
    // ACCOUNT_SERVICE_ENV.
    //
    // **The server list is built only when the list is what decides the address**, and
    // that is the one place the two plugins come apart. With `--server` the address was
    // typed and the world was named, so there is no row to click and no list to read —
    // `ServerListPlugin` would open a socket for an answer nothing would draw, and
    // `ui/servers.rs` draws nothing without the resource it inserts. `ServerListPlugin`
    // still reads the settings `SignInPlugin` inserts rather than keeping a second idea
    // of where this client signs in.
    if let Some(service) = account_service {
        match world {
            Some(world) => {
                app.add_plugins(SignInPlugin::new(service).for_world(world));
            }
            None => {
                app.add_plugins(SignInPlugin::new(service))
                    .add_plugins(ServerListPlugin);
            }
        }
    }

    app.run()
}

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    /// Start the app with these settings.
    Connect(Start),
    /// Print the usage text and stop.
    Usage,
}

/// Everything the app needs from the command line and the environment.
///
/// **At least one of `server_addr` and `account_service` is `Some`**, and which of the
/// three combinations [`parse_launch`] admits decides how the session is opened:
///
/// | `server_addr` | `account_service` | `world` | what happens                          |
/// | ------------- | ----------------- | ------- | ------------------------------------- |
/// | `Some`        | `None`            | `None`  | dialled at build, presenting nothing   |
/// | `None`        | `Some`            | `None`  | the login screen, then the server list |
/// | `Some`        | `Some`            | `Some`  | the login screen, then that address     |
///
/// Every other shape is a usage error, and the reasons are on the match in
/// [`parse_launch`] that produces them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Start {
    /// The address to dial, already carrying a port. `None` means the server list
    /// decides, which is the path a player takes.
    server_addr: Option<String>,
    /// A display name. Never an identity: the token is that, and the server mints
    /// it.
    player_name: String,
    /// The character to ask for, when `--name` or `VOXELHEIM_NAME` named one.
    ///
    /// `None` is the ordinary launch, where the character screen waits for a person.
    /// It is separate from [`Self::player_name`] because that one is never absent —
    /// it falls back to [`DEFAULT_PLAYER_NAME`] — and a default is not a request:
    /// a client that asked to play "voxelheim" would create a character nobody named.
    chosen_character: Option<String>,
    /// `--identity`, which replaces the per-server file outright. `None` leaves
    /// the choice to `net/session.rs`, which is the only code that knows where a
    /// token belongs.
    identity_path: Option<PathBuf>,
    /// `--account-service`, already parsed. `Some` is a launch that signs in: with no
    /// address it is the path a player takes — the login screen, then the server list,
    /// then a row carrying both the address and the certificate to expect at it — and
    /// with one it is the development path.
    account_service: Option<AccountService>,
    /// `--world`, which names the world a ticket is asked for. `Some` exactly when both
    /// of the two above are, because it is the one path where nothing else can say
    /// which world an address is running.
    world: Option<String>,
}

/// The environment the launch settings fall back to, read once in [`main`].
///
/// A struct rather than three parameters so that adding a fourth setting later does
/// not re-thread every call, and *passed in* rather than read from the process, so
/// [`parse_launch`] stays pure — the precedence rules are then unit-testable without
/// an environment to mutate, which Rust 2024 makes `unsafe` anyway.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LaunchEnv {
    server_addr: Option<String>,
    player_name: Option<String>,
    identity_path: Option<String>,
    account_service: Option<String>,
    account_service_fingerprint: Option<String>,
    world: Option<String>,
}

/// Which setting an argument was naming, for the two errors that report it.
///
/// The words are the ones a player reads in `voxelheim-client: the ... is empty`,
/// so they are prose rather than flag spellings.
const SERVER_ADDR: &str = "server address";
const PLAYER_NAME: &str = "player name";
const IDENTITY_PATH: &str = "identity file";
const ACCOUNT_SERVICE: &str = "account service";
const ACCOUNT_SERVICE_FINGERPRINT: &str = "account service fingerprint";
const WORLD: &str = "world";

/// Resolves every launch setting from, in order of precedence: the command line,
/// the environment, then the built-in default.
///
/// Hand-rolled rather than reached for a crate, because the argument surface is
/// three options and the dependency budget for this client is two crates (see
/// `client/AGENTS.md`). Pure, so the precedence rules are unit-testable without an
/// environment or a process.
///
/// An option given twice is an error rather than last-one-wins: two values in one
/// command line means one of them is not what was meant, and choosing silently is
/// how a player ends up on the wrong character. An **empty** value is an error for
/// the same reason — but only from the command line. An exported-but-empty
/// variable is an unset one, since a shell exports those by accident.
fn parse_launch(args: &[String], env: &LaunchEnv) -> Result<Launch, String> {
    let mut server_addr: Option<String> = None;
    let mut player_name: Option<String> = None;
    let mut identity_path: Option<String> = None;
    let mut account_service: Option<String> = None;
    let mut account_service_fingerprint: Option<String> = None;
    let mut world: Option<String> = None;
    let mut args = args.iter();

    while let Some(arg) = args.next() {
        // The value that follows a separated flag, as in `--name thora`. Named
        // rather than repeated three times, and it reports the *kind* of value it
        // wanted so a dangling flag says something better than "needs a value".
        let mut after = |wanted: &str| -> Result<String, String> {
            args.next()
                .ok_or_else(|| format!("{arg} needs {wanted}"))
                .cloned()
        };

        let (slot, what, value) = match arg.as_str() {
            "-h" | "--help" => return Ok(Launch::Usage),
            "-s" | "--server" => (&mut server_addr, SERVER_ADDR, after("an address")?),
            "-n" | "--name" => (&mut player_name, PLAYER_NAME, after("a name")?),
            "-i" | "--identity" => (&mut identity_path, IDENTITY_PATH, after("a path")?),
            "-a" | "--account-service" => (&mut account_service, ACCOUNT_SERVICE, after("a URL")?),
            // Long-only, deliberately. Every short flag here is the first letter of what
            // it names and `-a` is taken; a second letter chosen for this one would be a
            // letter nobody guesses right, and it is typed once per machine rather than
            // once per launch.
            "--account-service-fingerprint" => (
                &mut account_service_fingerprint,
                ACCOUNT_SERVICE_FINGERPRINT,
                after("a SHA-256")?,
            ),
            "-w" | "--world" => (&mut world, WORLD, after("a world name")?),
            other => {
                if let Some(value) = other.strip_prefix("--server=") {
                    (&mut server_addr, SERVER_ADDR, value.to_owned())
                } else if let Some(value) = other.strip_prefix("--name=") {
                    (&mut player_name, PLAYER_NAME, value.to_owned())
                } else if let Some(value) = other.strip_prefix("--identity=") {
                    (&mut identity_path, IDENTITY_PATH, value.to_owned())
                } else if let Some(value) = other.strip_prefix("--account-service-fingerprint=") {
                    (
                        &mut account_service_fingerprint,
                        ACCOUNT_SERVICE_FINGERPRINT,
                        value.to_owned(),
                    )
                } else if let Some(value) = other.strip_prefix("--account-service=") {
                    (&mut account_service, ACCOUNT_SERVICE, value.to_owned())
                } else if let Some(value) = other.strip_prefix("--world=") {
                    (&mut world, WORLD, value.to_owned())
                // Anything else that looks like a flag is a typo worth reporting;
                // silently ignoring it would hide a misspelled --server.
                } else if other.starts_with('-') {
                    return Err(format!("unknown option {other}"));
                } else {
                    (&mut server_addr, SERVER_ADDR, other.to_owned())
                }
            }
        };

        if slot.is_some() {
            return Err(format!("the {what} was given twice, second was {value}"));
        }
        if value.trim().is_empty() {
            return Err(format!("the {what} is empty"));
        }
        *slot = Some(value);
    }

    // An exported-but-empty variable is an unset one. Treating it as a value would
    // turn `VOXELHEIM_SERVER=` into a connection failure with no clue, and
    // `VOXELHEIM_NAME=` into a nameless player.
    let given_addr = server_addr.or_else(|| exported(env.server_addr.as_deref()));
    // Given rather than defaulted, because the two mean different things below: the
    // display name always has a value, and the character to ask for exists only when
    // somebody actually named one.
    let given_name = player_name.or_else(|| exported(env.player_name.as_deref()));
    let name = given_name
        .clone()
        .unwrap_or_else(|| DEFAULT_PLAYER_NAME.to_owned());
    // The path is trimmed like everything else here. A file name with a leading or
    // trailing space is legal on Unix and is therefore information being thrown away
    // — but it is information a player virtually never means, while a stray space
    // around a pasted path is something they do all the time, and the failure it
    // causes ("no such file", naming a path that looks right) is a bad one.
    let identity = identity_path
        .or_else(|| exported(env.identity_path.as_deref()))
        .map(|path| PathBuf::from(path.trim()));
    // Parsed here rather than in the plugin, so a mistyped URL is a usage error
    // before a window opens instead of a refusal on a login screen. It is also the
    // one place `http` is turned away — see `AccountService::parse`.
    //
    // **The address and its fingerprint are resolved together and refused together**,
    // because an address with nothing to check the certificate against is the hole #131
    // closed and a fingerprint with no address is a launch that means something other
    // than what was typed. Neither is defaulted: there is no number this client could
    // guess and no service it could discover one from — this connection is where its
    // trust begins.
    let given_service = account_service.or_else(|| exported(env.account_service.as_deref()));
    let given_print = account_service_fingerprint
        .or_else(|| exported(env.account_service_fingerprint.as_deref()));
    let account = match (given_service, given_print) {
        (Some(raw), Some(fingerprint)) => {
            Some(AccountService::parse(raw.trim(), fingerprint.trim())?)
        }
        (Some(raw), None) => {
            return Err(format!(
                "an account service was given ({}) with no fingerprint to expect at it. \
                 That service prints one at every start, as certificate_sha256; pass it \
                 with --account-service-fingerprint. This client checks it instead of a \
                 certificate authority, and a connection it cannot check is one anybody \
                 on the way can answer — including with a signing key of their own.",
                raw.trim()
            ));
        }
        (None, Some(_)) => {
            return Err(
                "an account service fingerprint was given with no --account-service \
                 to reach. Give the service's https URL beside it, or drop it."
                    .to_owned(),
            );
        }
        (None, None) => None,
    };

    let world = world
        .or_else(|| exported(env.world.as_deref()))
        .map(|world| world.trim().to_owned());

    // **Three launches, and which one this is decides what may be presented and to
    // whom.** The address and the account service are not a setting and an override:
    // an account service with no address means every address comes from its list,
    // carrying the certificate to expect at it, while an address given here is in no
    // list and can be verified against nothing.
    //
    // They were mutually exclusive until #154, on the argument that a stored credential
    // must not be handed to an address nobody stated a certificate for. The argument
    // was right about the credential it was written for — a token that names a player
    // at one server until somebody deletes it — and it does not carry to the one this
    // now presents. A session ticket names one world, expires in hours, and is refused
    // at every other world, so an unverified address learns one world's session for an
    // afternoon, at an address the developer typed. The alternative was that
    // development could not connect at all: a hello with no ticket is refused, and it
    // is meant to be.
    //
    // What did not change is the verification. An address given here is still
    // `Unlisted`, still verified against nothing, and still says so; a listed server is
    // still refused before a byte is sent when its certificate is not the one the list
    // carried.
    let addr = match (given_addr, &account, &world) {
        // The development path, and the only one that both names an address and can
        // sign in. `--world` is what a list row would have carried; see
        // `net::SignInPlugin::for_world` for why nothing infers it from the address.
        (Some(addr), Some(_), Some(_)) => Some(with_default_port(addr.trim())),
        // An address and a service with no world. Not defaulted and not guessed: a
        // ticket names exactly one world, this launch would have to pick which, and a
        // wrong guess is a refusal the player cannot read a remedy out of.
        (Some(addr), Some(_), None) => {
            return Err(format!(
                "an account service and a server address were both given ({}), but no world. \
                 A ticket names one world and is refused at every other, so this client has \
                 to be told which world that address is running: add --world NAME, the same \
                 name the server was started with as -world-name.",
                addr.trim()
            ));
        }
        // An address alone. Nothing to sign in against, so the hello presents no
        // account and the server refuses it — which it says, so this is not a usage
        // error. See DEFAULT_SERVER_ADDR.
        (Some(addr), None, None) => Some(with_default_port(addr.trim())),
        // A world with no address is a world nothing would ask for. On the list path
        // the row that is clicked names the world, so a `--world` here would either be
        // ignored — the silent precedence rule this file refuses everywhere else — or
        // would have to override a row, which is a way to ask for a ticket for one
        // world and present it at another.
        (None, Some(_), Some(world)) => {
            return Err(format!(
                "a world was given ({world}) with an account service and no address. The \
                 server list is where worlds come from on that path: the row you click names \
                 one. Give --server as well to name both yourself."
            ));
        }
        (None, Some(_), None) => None,
        // No service and no address: the development default, which is a `voxelheimd`
        // running beside this one.
        (None, None, None) => Some(DEFAULT_SERVER_ADDR.to_owned()),
        // A world with nothing to ask one for. Refused rather than ignored, whether or
        // not an address came with it: the launch this was meant to be is one flag
        // away, and connecting without it produces a refusal from the server that says
        // nothing about the flag that was typed.
        (_, None, Some(world)) => {
            return Err(format!(
                "a world was given ({world}) with no account service. A ticket for a world \
                 comes from signing in, so name where to sign in with --account-service URL."
            ));
        }
    };

    Ok(Launch::Connect(Start {
        server_addr: addr,
        player_name: name.trim().to_owned(),
        chosen_character: given_name.map(|name| name.trim().to_owned()),
        identity_path: identity,
        account_service: account,
        world,
    }))
}

/// The value of an environment variable that was actually set to something.
fn exported(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

/// Gives a bare host the default port.
///
/// A value already containing a colon is passed through untouched, which covers
/// `host:port` and the bracketed IPv6 form `[::1]:7777`. A bare IPv6 literal is
/// left alone and will fail to resolve — std cannot tell `::1` from a host called
/// `` with port `:1` either, and inventing a rule here would only disagree with it.
fn with_default_port(addr: &str) -> String {
    if addr.contains(':') {
        addr.to_owned()
    } else {
        format!("{addr}:{DEFAULT_SERVER_PORT}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// An environment with nothing exported, which is what most of these want.
    fn nothing() -> LaunchEnv {
        LaunchEnv::default()
    }

    /// A well-formed account service address, and the fingerprint that has to travel with
    /// it. Neither is reachable in a test; what they have to be is well-formed.
    const SERVICE_URL: &str = "https://127.0.0.1:7780";
    const SERVICE_PIN: &str = "abababababababababababababababababababababababababababababababab";

    /// An environment exporting that fingerprint and nothing else.
    ///
    /// The fingerprint travels with the address rather than instead of it, and most of
    /// the cases below are about the address — so exporting the number keeps them about
    /// what they are about. The pairing itself is
    /// [`an_account_service_and_its_fingerprint_travel_together`].
    fn pinned() -> LaunchEnv {
        LaunchEnv {
            account_service_fingerprint: Some(SERVICE_PIN.to_owned()),
            ..LaunchEnv::default()
        }
    }

    fn start(raw: &[&str], env: &LaunchEnv) -> Result<Start, String> {
        match parse_launch(&args(raw), env)? {
            Launch::Connect(start) => Ok(start),
            Launch::Usage => Err("expected a launch, got a usage request".to_owned()),
        }
    }

    fn connect(raw: &[&str], env: Option<&str>) -> Result<String, String> {
        let env = LaunchEnv {
            server_addr: env.map(str::to_owned),
            ..LaunchEnv::default()
        };
        start(raw, &env).map(|start| {
            start
                .server_addr
                .expect("no account service was given, so an address was resolved")
        })
    }

    fn name(raw: &[&str], env: Option<&str>) -> Result<String, String> {
        let env = LaunchEnv {
            player_name: env.map(str::to_owned),
            ..LaunchEnv::default()
        };
        start(raw, &env).map(|start| start.player_name)
    }

    fn identity(raw: &[&str], env: Option<&str>) -> Result<Option<PathBuf>, String> {
        let env = LaunchEnv {
            identity_path: env.map(str::to_owned),
            ..LaunchEnv::default()
        };
        start(raw, &env).map(|start| start.identity_path)
    }

    /// The account service a launch resolved to, as it renders.
    ///
    /// `env` is an exported address, and the fingerprint is exported with it: the two are
    /// one setting given in two places, and a helper that exported one without the other
    /// would make every case below a test of the pairing rule instead of of the address.
    fn account(raw: &[&str], env: Option<&str>) -> Result<Option<String>, String> {
        let env = LaunchEnv {
            account_service: env.map(str::to_owned),
            account_service_fingerprint: env.map(|_| SERVICE_PIN.to_owned()),
            ..LaunchEnv::default()
        };
        start(raw, &env).map(|start| start.account_service.map(|service| service.to_string()))
    }

    #[test]
    fn nothing_given_means_localhost() {
        assert_eq!(connect(&[], None), Ok(DEFAULT_SERVER_ADDR.to_owned()));
    }

    #[test]
    fn the_environment_supplies_a_default() {
        assert_eq!(
            connect(&[], Some("norse.example:9000")),
            Ok("norse.example:9000".to_owned())
        );
    }

    #[test]
    fn an_empty_environment_variable_is_not_an_address() {
        assert_eq!(connect(&[], Some("")), Ok(DEFAULT_SERVER_ADDR.to_owned()));
        assert_eq!(
            connect(&[], Some("   ")),
            Ok(DEFAULT_SERVER_ADDR.to_owned())
        );
    }

    #[test]
    fn the_command_line_beats_the_environment() {
        assert_eq!(
            connect(&["192.0.2.5:7000"], Some("norse.example:9000")),
            Ok("192.0.2.5:7000".to_owned())
        );
    }

    #[test]
    fn every_spelling_of_the_option_works() {
        for raw in [
            vec!["192.0.2.5:7000"],
            vec!["--server", "192.0.2.5:7000"],
            vec!["-s", "192.0.2.5:7000"],
            vec!["--server=192.0.2.5:7000"],
        ] {
            assert_eq!(
                connect(&raw, None),
                Ok("192.0.2.5:7000".to_owned()),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn a_bare_host_gets_the_default_port() {
        assert_eq!(
            connect(&["norse.example"], None),
            Ok("norse.example:7777".to_owned())
        );
        assert_eq!(
            connect(&["localhost"], None),
            Ok("localhost:7777".to_owned())
        );
    }

    #[test]
    fn an_address_with_a_port_is_left_alone() {
        assert_eq!(connect(&["[::1]:7777"], None), Ok("[::1]:7777".to_owned()));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            connect(&[], Some("  norse.example:9000  ")),
            Ok("norse.example:9000".to_owned())
        );
    }

    #[test]
    fn help_is_answered_without_starting_the_app() {
        for flag in ["-h", "--help"] {
            assert_eq!(parse_launch(&args(&[flag]), &nothing()), Ok(Launch::Usage));
        }
        // Even alongside an address: asking for help is never a connection.
        assert_eq!(
            parse_launch(&args(&["192.0.2.5:7000", "--help"]), &nothing()),
            Ok(Launch::Usage)
        );
    }

    #[test]
    fn a_dangling_server_option_is_an_error() {
        for flag in ["-s", "--server"] {
            let err = parse_launch(&args(&[flag]), &nothing()).expect_err("no address was given");
            assert!(err.contains("needs an address"), "{err}");
        }
    }

    #[test]
    fn a_misspelled_option_is_an_error_rather_than_an_address() {
        let err = parse_launch(&args(&["--sevrer=host"]), &nothing()).expect_err("that is a typo");
        assert!(err.contains("unknown option"), "{err}");
    }

    #[test]
    fn two_addresses_are_an_error() {
        let err = parse_launch(&args(&["a:1", "b:2"]), &nothing()).expect_err("which one?");
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn an_empty_address_is_an_error() {
        let err =
            parse_launch(&args(&["--server="]), &nothing()).expect_err("that is not an address");
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn nothing_given_means_the_default_name() {
        assert_eq!(name(&[], None), Ok(DEFAULT_PLAYER_NAME.to_owned()));
    }

    #[test]
    fn every_spelling_of_the_name_option_works() {
        for raw in [
            vec!["--name", "thora"],
            vec!["-n", "thora"],
            vec!["--name=thora"],
        ] {
            assert_eq!(name(&raw, None), Ok("thora".to_owned()), "{raw:?}");
        }
    }

    #[test]
    fn the_command_line_name_beats_the_environment() {
        assert_eq!(
            name(&["--name", "thora"], Some("bjorn")),
            Ok("thora".to_owned())
        );
        assert_eq!(name(&[], Some("bjorn")), Ok("bjorn".to_owned()));
    }

    #[test]
    fn an_empty_name_on_the_command_line_is_an_error() {
        // Asked for explicitly, so it is a usage error rather than a default: a
        // player who typed `--name=` meant something, and it was not "voxelheim".
        for raw in [vec!["--name="], vec!["--name", "   "], vec!["-n", ""]] {
            let err = name(&raw, None).expect_err("that is not a name");
            assert!(err.contains("player name"), "{raw:?} -> {err}");
            assert!(err.contains("empty"), "{raw:?} -> {err}");
        }
    }

    #[test]
    fn an_empty_environment_name_is_not_a_name() {
        // Same rule as VOXELHEIM_SERVER, and for the same reason: a shell exports
        // an empty variable by accident, and a nameless player is not what it meant.
        for exported in ["", "   "] {
            assert_eq!(
                name(&[], Some(exported)),
                Ok(DEFAULT_PLAYER_NAME.to_owned()),
                "{exported:?}"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_from_a_name() {
        assert_eq!(name(&["--name", "  thora  "], None), Ok("thora".to_owned()));
    }

    #[test]
    fn no_identity_option_leaves_the_choice_to_the_net_thread() {
        // `None` is not "no identity": it means the per-server file, which only
        // `net/session.rs` knows how to name.
        assert_eq!(identity(&[], None), Ok(None));
    }

    #[test]
    fn every_spelling_of_the_identity_option_works() {
        for raw in [
            vec!["--identity", "/tmp/one"],
            vec!["-i", "/tmp/one"],
            vec!["--identity=/tmp/one"],
        ] {
            assert_eq!(
                identity(&raw, None),
                Ok(Some(PathBuf::from("/tmp/one"))),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn the_command_line_identity_beats_the_environment() {
        assert_eq!(
            identity(&["--identity", "/tmp/one"], Some("/tmp/two")),
            Ok(Some(PathBuf::from("/tmp/one")))
        );
        assert_eq!(
            identity(&[], Some("/tmp/two")),
            Ok(Some(PathBuf::from("/tmp/two")))
        );
    }

    #[test]
    fn an_empty_identity_path_is_an_error_on_the_command_line_and_unset_in_the_environment() {
        let err = identity(&["--identity="], None).expect_err("that is not a path");
        assert!(err.contains("identity file"), "{err}");
        assert!(err.contains("empty"), "{err}");

        assert_eq!(identity(&[], Some("  ")), Ok(None));
    }

    #[test]
    fn a_dangling_option_names_what_it_wanted() {
        for (flag, wanted) in [
            ("--server", "an address"),
            ("-s", "an address"),
            ("--name", "a name"),
            ("-n", "a name"),
            ("--identity", "a path"),
            ("-i", "a path"),
            ("--account-service", "a URL"),
            ("-a", "a URL"),
        ] {
            let err = parse_launch(&args(&[flag]), &nothing()).expect_err("nothing followed it");
            assert!(err.contains(wanted), "{flag} -> {err}");
        }
    }

    #[test]
    fn every_option_given_twice_is_an_error() {
        // Last-one-wins would pick silently, and picking silently between two
        // identity files is how a player ends up on the wrong character.
        for raw in [
            vec!["--name", "thora", "--name", "bjorn"],
            vec!["--identity=/tmp/one", "-i", "/tmp/two"],
            vec!["--server=a:1", "-s", "b:2"],
        ] {
            let err = parse_launch(&args(&raw), &nothing()).expect_err("which one?");
            assert!(err.contains("twice"), "{raw:?} -> {err}");
        }
    }

    #[test]
    fn the_three_settings_do_not_interfere() {
        let start = start(
            &["norse.example", "--name", "thora", "--identity=/tmp/one"],
            &LaunchEnv {
                server_addr: Some("ignored:1".to_owned()),
                player_name: Some("ignored".to_owned()),
                identity_path: Some("/tmp/ignored".to_owned()),
                account_service: None,
                account_service_fingerprint: None,
                world: None,
            },
        )
        .expect("a complete command line");

        assert_eq!(
            start,
            Start {
                server_addr: Some("norse.example:7777".to_owned()),
                player_name: "thora".to_owned(),
                chosen_character: Some("thora".to_owned()),
                identity_path: Some(PathBuf::from("/tmp/one")),
                account_service: None,
                world: None,
            }
        );
    }

    #[test]
    fn help_still_wins_over_every_other_option() {
        assert_eq!(
            parse_launch(
                &args(&["--name", "thora", "--identity=/tmp/one", "-h"]),
                &nothing()
            ),
            Ok(Launch::Usage)
        );
    }

    #[test]
    fn the_usage_text_documents_every_option_and_variable() {
        // The usage text is the only documentation a player gets, and an option
        // that is not in it may as well not exist.
        for mentioned in [
            "--server",
            "--name",
            "--identity",
            "VOXELHEIM_SERVER",
            "VOXELHEIM_NAME",
            "VOXELHEIM_IDENTITY",
            "--account-service",
            "VOXELHEIM_ACCOUNT_SERVICE",
            "--account-service-fingerprint",
            "VOXELHEIM_ACCOUNT_SERVICE_FINGERPRINT",
            "--world",
            "VOXELHEIM_WORLD",
            // The development path is documented as one, which is the acceptance
            // criterion this line stands for: a player must not read `--server` as
            // the ordinary way in.
            "development path",
        ] {
            assert!(
                USAGE.contains(mentioned),
                "the usage text omits {mentioned}"
            );
        }
    }

    /// **A name given is a character asked for; a name defaulted is not.**
    ///
    /// The two travel together and mean different things: the hello's display name
    /// always has a value, and the request only exists when somebody named one. A
    /// launch that asked to play [`DEFAULT_PLAYER_NAME`] would have a character called
    /// "voxelheim" created for it on every fresh world.
    #[test]
    fn only_a_name_that_was_given_asks_for_a_character() {
        let launched = |raw: &[&str], env: Option<&str>| {
            start(
                raw,
                &LaunchEnv {
                    player_name: env.map(str::to_owned),
                    ..LaunchEnv::default()
                },
            )
            .expect("a legal command line")
        };

        let defaulted = launched(&["norse.example"], None);
        assert_eq!(defaulted.player_name, DEFAULT_PLAYER_NAME);
        assert_eq!(
            defaulted.chosen_character, None,
            "nobody was named, so the screen asks"
        );

        for start in [
            launched(&["norse.example", "--name", "  thora  "], None),
            launched(&["norse.example", "--name=thora"], None),
            launched(&["norse.example"], Some("thora")),
        ] {
            assert_eq!(start.chosen_character.as_deref(), Some("thora"));
            assert_eq!(
                start.player_name, "thora",
                "the same name still reaches the hello"
            );
        }
    }

    /// **The combination #150 forbade is the development path #154 needs**, and what
    /// makes it admissible is the third value: the world a ticket is asked for. Without
    /// it there is still nothing this client could do but guess.
    #[test]
    fn an_account_service_and_an_address_need_a_world_between_them() {
        for both in [
            vec![
                "--account-service",
                SERVICE_URL,
                "--server",
                "server.example:7777",
            ],
            vec!["server.example:7777", "-a", SERVICE_URL],
        ] {
            let err = start(&both, &pinned()).expect_err("a world has to be named");
            assert!(err.contains("--world"), "{err}");
            assert!(err.contains("server.example:7777"), "{err}");
        }

        // An exported address reaches the same rule from the other direction: it is a
        // value somebody set, and the refusal names it rather than quietly winning or
        // quietly losing.
        let err = start(
            &["--account-service", SERVICE_URL],
            &LaunchEnv {
                server_addr: Some("server.example:7777".to_owned()),
                ..pinned()
            },
        )
        .expect_err("an exported address was accepted with no world");
        assert!(err.contains("server.example:7777"), "{err}");
    }

    /// The launch this issue exists for: sign in, and connect to the address that was
    /// typed rather than to a row of a list.
    #[test]
    fn an_account_service_an_address_and_a_world_are_the_development_path() {
        let start = start(
            &[
                "--account-service",
                SERVICE_URL,
                "--server",
                "127.0.0.1:7777",
                "--world",
                "midgard",
            ],
            &pinned(),
        )
        .expect("all three together are a complete command line");

        assert_eq!(start.server_addr.as_deref(), Some("127.0.0.1:7777"));
        assert_eq!(start.world.as_deref(), Some("midgard"));
        assert!(start.account_service.is_some());
    }

    #[test]
    fn every_spelling_of_the_world_option_works() {
        for raw in [
            vec!["--world", "midgard"],
            vec!["-w", "midgard"],
            vec!["--world=midgard"],
        ] {
            let mut whole = vec!["-a", SERVICE_URL, "-s", "127.0.0.1:7777"];
            whole.extend(raw.iter().copied());
            let start = start(&whole, &pinned()).expect("a complete command line");
            assert_eq!(start.world.as_deref(), Some("midgard"), "{raw:?}");
        }
    }

    /// Lower precedence than the command line, and an exported empty value is an unset
    /// one — the rule every other setting here follows.
    #[test]
    fn the_command_line_world_beats_the_environment() {
        let both = LaunchEnv {
            world: Some("exported".to_owned()),
            ..pinned()
        };
        let typed = start(
            &[
                "-a",
                SERVICE_URL,
                "-s",
                "127.0.0.1:7777",
                "--world",
                "typed",
            ],
            &both,
        )
        .expect("a complete command line");
        assert_eq!(typed.world.as_deref(), Some("typed"));

        let exported = start(&["-a", SERVICE_URL, "-s", "127.0.0.1:7777"], &both)
            .expect("an exported world is enough");
        assert_eq!(exported.world.as_deref(), Some("exported"));
    }

    /// **A world nothing would ask for is refused rather than ignored.** Both shapes
    /// are a command line one flag away from a launch that works, and a client that
    /// dropped the value silently would leave the player reading a refusal from the
    /// server that says nothing about the flag they typed.
    #[test]
    fn a_world_with_nothing_to_ask_for_one_is_a_usage_error() {
        // No account service: nothing can mint a ticket for a world.
        let err = start(
            &["--server", "127.0.0.1:7777", "--world", "midgard"],
            &nothing(),
        )
        .expect_err("a world with no service was accepted");
        assert!(err.contains("--account-service"), "{err}");

        // A service but no address: the row that is clicked names the world, so a
        // second answer here could only disagree with it.
        let err = start(
            &["--account-service", SERVICE_URL, "-w", "midgard"],
            &pinned(),
        )
        .expect_err("a world with no address was accepted");
        assert!(err.contains("--server"), "{err}");
    }

    /// An address with no account service still launches: there is nothing to sign in
    /// against, the hello presents no account, and the server says so. Refusing it here
    /// would be this client answering for the server.
    #[test]
    fn an_address_alone_is_still_a_launch() {
        let start = start(&["127.0.0.1:7777"], &nothing()).expect("an address alone");
        assert_eq!(start.server_addr.as_deref(), Some("127.0.0.1:7777"));
        assert_eq!(start.world, None);
        assert!(start.account_service.is_none());
    }

    /// With a service, the address is the list's to decide and this client resolves
    /// none — not even the default, which would be an address nobody asked for.
    #[test]
    fn an_account_service_leaves_the_address_to_the_list() {
        let launched = start(&["--account-service", SERVICE_URL], &pinned())
            .expect("a service alone is a complete command line");
        assert_eq!(launched.server_addr, None);
        assert!(launched.account_service.is_some());
    }

    #[test]
    fn no_account_service_means_no_sign_in_at_all() {
        // The default, and deliberately not a degraded mode: a client given no
        // service is the client this repository had before signing in existed.
        assert_eq!(account(&[], None), Ok(None));
    }

    #[test]
    fn every_spelling_of_the_account_service_option_works() {
        let long_pin = format!("--account-service-fingerprint={SERVICE_PIN}");
        for raw in [
            vec![
                "--account-service",
                SERVICE_URL,
                "--account-service-fingerprint",
                SERVICE_PIN,
            ],
            vec![
                "-a",
                SERVICE_URL,
                "--account-service-fingerprint",
                SERVICE_PIN,
            ],
            vec![&format!("--account-service={SERVICE_URL}"), &long_pin],
        ] {
            assert_eq!(
                account(&raw, None),
                Ok(Some(SERVICE_URL.to_owned())),
                "{raw:?}"
            );
        }
        assert_eq!(
            account(&[], Some("https://accounts.example:7780")),
            Ok(Some("https://accounts.example:7780".to_owned()))
        );
        // The address from the command line, the fingerprint from the environment: the
        // two are resolved independently, so a machine can export the number once and
        // still name a different service on one launch.
        assert_eq!(
            account(&["-a", "https://one.example"], Some("https://two.example")),
            Ok(Some("https://one.example:443".to_owned()))
        );
    }

    /// **Neither half of the anchor is optional, and neither is silently ignored** (#131).
    /// An address with nothing to check the certificate against is the hole this closed;
    /// a fingerprint with no address is a launch that means something other than what was
    /// typed, and dropping it quietly is how a mistyped `--account-service` becomes a
    /// client that never signs in and never says why.
    #[test]
    fn an_account_service_and_its_fingerprint_travel_together() {
        let err = start(&["-a", SERVICE_URL], &nothing())
            .expect_err("an account service with no fingerprint was accepted");
        assert!(err.contains("--account-service-fingerprint"), "{err}");
        assert!(err.contains(SERVICE_URL), "{err}");

        let err = start(&["--account-service-fingerprint", SERVICE_PIN], &nothing())
            .expect_err("a fingerprint with no account service was accepted");
        assert!(err.contains("--account-service"), "{err}");

        // And the exported spellings reach the same rule, because they are the same
        // setting arriving somewhere else.
        assert!(
            start(&[], &pinned()).is_err(),
            "an exported fingerprint with no service was accepted"
        );
        assert!(
            start(
                &[],
                &LaunchEnv {
                    account_service: Some(SERVICE_URL.to_owned()),
                    ..LaunchEnv::default()
                }
            )
            .is_err(),
            "an exported service with no fingerprint was accepted"
        );
    }

    #[test]
    fn a_url_the_client_cannot_use_is_a_usage_error_rather_than_a_login_screen() {
        // Caught before a window opens, which is the whole reason the URL is parsed
        // in here rather than in the plugin.
        for raw in ["accounts.example", "ftp://accounts.example", "https://"] {
            assert!(
                account(
                    &["-a", raw, "--account-service-fingerprint", SERVICE_PIN],
                    None
                )
                .is_err(),
                "{raw}"
            );
        }
        // **The plaintext one is the refusal #131 added**, and it is a usage error rather
        // than a downgrade: the account service has no plaintext listener to reach.
        let err = account(
            &[
                "-a",
                "http://accounts.example",
                "--account-service-fingerprint",
                SERVICE_PIN,
            ],
            None,
        )
        .expect_err("plaintext was accepted");
        assert!(err.contains("listens over TLS"), "{err}");

        // And a fingerprint that is not a digest, which is the other half of the same
        // anchor and the one a typo produces.
        let err = account(
            &["-a", SERVICE_URL, "--account-service-fingerprint", "nope"],
            None,
        )
        .expect_err("a malformed fingerprint was accepted");
        assert!(err.contains("SHA-256"), "{err}");
    }

    #[test]
    fn the_default_address_matches_the_servers_default_listener() {
        // server/cmd/voxelheimd/main.go: -listen defaults to 127.0.0.1:7777.
        assert_eq!(DEFAULT_SERVER_ADDR, "127.0.0.1:7777");
        assert_eq!(
            with_default_port("127.0.0.1"),
            DEFAULT_SERVER_ADDR,
            "the bare-host default and the full default must agree"
        );
    }
}
