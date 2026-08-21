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
mod ui;
mod world;

use std::path::PathBuf;
use std::process::ExitCode;

use bevy::prelude::*;

use crate::net::{AccountService, DEFAULT_PLAYER_NAME, NetPlugin, ServerListPlugin, SignInPlugin};
use crate::player::PlayerPlugin;
use crate::ui::UiPlugin;
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

/// Environment variable holding the account service URL, with the same precedence.
///
/// There is deliberately **no default**. An account service is something an
/// operator runs, and pointing at one that is not there would put a login screen in
/// front of every launch with nothing behind it — so signing in is opt-in, and a
/// client given no service behaves exactly as it did before signing in existed.
const ACCOUNT_SERVICE_ENV: &str = "VOXELHEIM_ACCOUNT_SERVICE";

const USAGE: &str = "\
Voxelheim client

Usage:
  voxelheim-client --account-service URL
  voxelheim-client [ADDRESS]
  voxelheim-client --server ADDRESS
  voxelheim-client --help

Arguments:
  ADDRESS         host:port of a server to develop against. A bare host gets
                  port 7777.

Options:
  -a, --account-service
                  URL of the account service to sign in against. The servers you
                  can join come from its list; you never type an address.
  -s, --server    the same address as ADDRESS, named explicitly
  -n, --name      the display name announced to the server
  -i, --identity  file holding this server's identity token
  -h, --help      print this and exit

Environment:
  VOXELHEIM_ACCOUNT_SERVICE  used when --account-service is not given
  VOXELHEIM_SERVER           used when no address is given on the command line
  VOXELHEIM_NAME             used when --name is not given
  VOXELHEIM_IDENTITY         used when --identity is not given

There are two ways to reach a server and you give exactly one of them.

  With --account-service, you sign in with Discord once and then pick a server
  out of the list it answers with. The list carries each server's address and
  the fingerprint of the certificate it presents, so the address is followed if
  it moves and a server presenting anything else is refused before this client
  sends a byte. There is no way past that refusal, by design: ask whoever runs
  the server to register the fingerprint it logs at startup.

  With an ADDRESS or --server, you connect straight to an address that is in no
  list. This is the development path. Nothing states which certificate to expect
  there, so the session is encrypted but unverified -- and for that reason it
  presents no stored identity and keeps none, which means a new character every
  time. Give an account service instead to play.

The name defaults to voxelheim; with neither an account service nor an address,
the address defaults to 127.0.0.1:7777.

The sign-in is kept in $XDG_DATA_HOME/voxelheim/account/<service>, falling back
to $HOME/.local/share. Deleting that file is how you sign out.

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
        identity_path,
        account_service,
    } = start;

    // The address is not in the title on the list path, and cannot be: nothing has
    // been chosen when the window opens. It is on the development path, where it was
    // typed on the command line and is the one thing distinguishing two windows.
    let title = match &server_addr {
        Some(addr) => format!("Voxelheim - {addr}"),
        None => "Voxelheim".to_owned(),
    };

    // Exactly one of the two, which `parse_launch` has already refused to let be both.
    // `developing_against` dials at build; `listening` waits for a row to be clicked.
    let net = match server_addr {
        Some(addr) => NetPlugin::developing_against(addr),
        None => NetPlugin::listening(),
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
    // The player before the UI: the player plugin owns the one camera, and `bevy_ui`
    // draws through it. See the module comment in player/camera.rs.
    .add_plugins(PlayerPlugin)
    .add_plugins(UiPlugin);

    // Built only when there is a service to sign in against, which is what makes the
    // login screen and the server list absent rather than broken on a client that has
    // none. See ACCOUNT_SERVICE_ENV. The two go together: the list is read with the
    // ticket a sign-in caches, and `ServerListPlugin` reads the settings this one
    // inserts rather than keeping a second idea of where this client signs in.
    if let Some(service) = account_service {
        app.add_plugins(SignInPlugin::new(service))
            .add_plugins(ServerListPlugin);
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
/// **Exactly one of `server_addr` and `account_service` is `Some`**, which
/// [`parse_launch`] enforces rather than leaving to the reader: they are two answers to
/// the same question — where does a server come from — and a client that had both would
/// have to pick one silently. Which it picked would decide whether a certificate is
/// verified, so it is a usage error instead.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Start {
    /// The development address, already carrying a port. `Some` exactly when no
    /// account service was given.
    server_addr: Option<String>,
    /// A display name. Never an identity: the token is that, and the server mints
    /// it.
    player_name: String,
    /// `--identity`, which replaces the per-server file outright. `None` leaves
    /// the choice to `net/session.rs`, which is the only code that knows where a
    /// token belongs.
    identity_path: Option<PathBuf>,
    /// `--account-service`, already parsed. `Some` is the path a player takes: the
    /// login screen, then the server list, then a row that carries both the address
    /// and the certificate to expect at it.
    account_service: Option<AccountService>,
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
}

/// Which setting an argument was naming, for the two errors that report it.
///
/// The words are the ones a player reads in `voxelheim-client: the ... is empty`,
/// so they are prose rather than flag spellings.
const SERVER_ADDR: &str = "server address";
const PLAYER_NAME: &str = "player name";
const IDENTITY_PATH: &str = "identity file";
const ACCOUNT_SERVICE: &str = "account service";

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
            other => {
                if let Some(value) = other.strip_prefix("--server=") {
                    (&mut server_addr, SERVER_ADDR, value.to_owned())
                } else if let Some(value) = other.strip_prefix("--name=") {
                    (&mut player_name, PLAYER_NAME, value.to_owned())
                } else if let Some(value) = other.strip_prefix("--identity=") {
                    (&mut identity_path, IDENTITY_PATH, value.to_owned())
                } else if let Some(value) = other.strip_prefix("--account-service=") {
                    (&mut account_service, ACCOUNT_SERVICE, value.to_owned())
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
    let name = player_name
        .or_else(|| exported(env.player_name.as_deref()))
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
    // one place `https` is turned away — see `AccountService::parse`.
    let account = account_service
        .or_else(|| exported(env.account_service.as_deref()))
        .map(|raw| AccountService::parse(raw.trim()))
        .transpose()?;

    // **Two ways to reach a server, and exactly one of them per launch.** They are not
    // a setting and an override: an account service means every address comes from its
    // list, carrying the certificate to expect at it, while an address given here is in
    // no list and can be verified against nothing. A client holding both would have to
    // choose silently, and the choice decides whether the session is verified — so it
    // is refused, the way an option given twice is, and for the same reason.
    let addr = match (given_addr, &account) {
        (Some(addr), Some(_)) => {
            return Err(format!(
                "both an account service and a server address were given ({}). The server \
                 list is where addresses come from; --server names one that is in no list \
                 and is the development path. Give one of the two.",
                addr.trim()
            ));
        }
        (Some(addr), None) => Some(with_default_port(addr.trim())),
        // No service and no address: the development default, which is a `voxelheimd`
        // running beside this one.
        (None, None) => Some(DEFAULT_SERVER_ADDR.to_owned()),
        (None, Some(_)) => None,
    };

    Ok(Launch::Connect(Start {
        server_addr: addr,
        player_name: name.trim().to_owned(),
        identity_path: identity,
        account_service: account,
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

    fn account(raw: &[&str], env: Option<&str>) -> Result<Option<String>, String> {
        let env = LaunchEnv {
            account_service: env.map(str::to_owned),
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
            },
        )
        .expect("a complete command line");

        assert_eq!(
            start,
            Start {
                server_addr: Some("norse.example:7777".to_owned()),
                player_name: "thora".to_owned(),
                identity_path: Some(PathBuf::from("/tmp/one")),
                account_service: None,
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

    /// **The two ways to reach a server are two, not one with an override.** Given
    /// both, a client would have to choose silently — and the choice decides whether
    /// the certificate is verified against anything, which is not a decision to make
    /// without saying so.
    #[test]
    fn an_account_service_and_an_address_together_are_a_usage_error() {
        for both in [
            vec![
                "--account-service",
                "http://127.0.0.1:7780",
                "--server",
                "server.example:7777",
            ],
            vec!["server.example:7777", "-a", "http://127.0.0.1:7780"],
        ] {
            let err = start(&both, &nothing()).expect_err("both were accepted");
            assert!(err.contains("account service"), "{err}");
            assert!(err.contains("--server"), "{err}");
        }

        // An exported address is the same conflict from the other direction: it is a
        // value somebody set, and the refusal names it rather than quietly winning or
        // quietly losing.
        let err = start(
            &["--account-service", "http://127.0.0.1:7780"],
            &LaunchEnv {
                server_addr: Some("server.example:7777".to_owned()),
                ..LaunchEnv::default()
            },
        )
        .expect_err("an exported address was accepted beside a service");
        assert!(err.contains("server.example:7777"), "{err}");
    }

    /// With a service, the address is the list's to decide and this client resolves
    /// none — not even the default, which would be an address nobody asked for.
    #[test]
    fn an_account_service_leaves_the_address_to_the_list() {
        let launched = start(&["--account-service", "http://127.0.0.1:7780"], &nothing())
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
        for raw in [
            vec!["--account-service", "http://127.0.0.1:7780"],
            vec!["-a", "http://127.0.0.1:7780"],
            vec!["--account-service=http://127.0.0.1:7780"],
        ] {
            assert_eq!(
                account(&raw, None),
                Ok(Some("http://127.0.0.1:7780".to_owned())),
                "{raw:?}"
            );
        }
        assert_eq!(
            account(&[], Some("http://accounts.example:7780")),
            Ok(Some("http://accounts.example:7780".to_owned()))
        );
        assert_eq!(
            account(&["-a", "http://one.example"], Some("http://two.example")),
            Ok(Some("http://one.example:80".to_owned()))
        );
    }

    #[test]
    fn a_url_the_client_cannot_use_is_a_usage_error_rather_than_a_login_screen() {
        // Caught before a window opens, which is the whole reason the URL is parsed
        // in here rather than in the plugin.
        for raw in ["accounts.example", "ftp://accounts.example", "http://"] {
            assert!(account(&["-a", raw], None).is_err(), "{raw}");
        }
        let err = account(&["-a", "https://accounts.example"], None)
            .expect_err("no root store, so no https");
        assert!(err.contains("verify a certificate"), "{err}");
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
