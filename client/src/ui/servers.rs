//! The server list: a row per server, and a line under them.
//!
//! It exists only when an account service is configured — `ServerList` is inserted by
//! `net::ServerListPlugin` and nothing else — so a client launched without one draws
//! nothing here, exactly as it draws no login screen. `Option<Res<ServerList>>` is what
//! encodes that, the same shape `show_login` and `show_menu` use.
//!
//! **There is never an empty screen.** A list that could not be read shows the reason
//! and a retry; a list that is genuinely empty says so in words. The distinction is the
//! whole point of this screen and it is `net::ServerList`'s two failure-shaped variants
//! that carry it: an empty list reads as *no servers exist*, which is a claim this
//! client must not make on the strength of a network error.
//!
//! Nothing here decides anything. A row writes a [`ConnectRequest`] naming the server;
//! the network boundary owns the socket, the address and the certificate to expect at
//! it. This module never learns an address — see `net/servers.rs` for why the accessor
//! does not exist.

use bevy::prelude::*;

use crate::net::{
    ConnectRequest, ConnectionState, ListedServer, RefreshServerList, ServerList, SignInState,
};

use super::login::login_is_up;
use super::{BUTTON, button_colour};

pub(super) struct ServerListUiPlugin;

impl Plugin for ServerListUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_server_list).add_systems(
            Update,
            (
                show_server_list,
                rebuild_rows,
                row_action,
                retry_action,
                refresh_server_list_text,
            ),
        );
    }
}

#[derive(Component)]
struct ServerListRoot;

/// The container the rows are spawned into and cleared out of.
#[derive(Component)]
struct ServerRows;

/// One row, carrying the name a [`ConnectRequest`] is written with — and nothing else.
#[derive(Component)]
struct ServerRow(String);

/// The one control that is always there: read the list again.
#[derive(Component)]
struct RetryButton;

/// The line under the rows: why there are none, or why the last connection did not
/// happen.
#[derive(Component)]
struct ServerListStatus;

/// Between the pause menu's 40 and the login screen's 50: a player who is not signed in
/// has nothing for this screen to be about, and a player who is has nothing for the
/// pause menu to be about yet.
const SERVERS_LAYER: i32 = 45;

/// An offline server's row, and it is dimmed rather than disabled. "Offline" is the
/// account service saying it has not heard from that server recently, which is not the
/// same as unreachable — nothing on either side dials it to find out — so the row stays
/// clickable and the word is the honest one.
const OFFLINE_LABEL: Color = Color::srgb(0.55, 0.59, 0.66);
const ONLINE_LABEL: Color = Color::WHITE;

const RETRY_LABEL: &str = "REFRESH THE LIST";

/// What the line says while the account service is being asked.
const LOADING: &str = "Reading the list of servers...";

/// And what it says when the answer was "none", which is a true answer.
const NO_SERVERS: &str = "No server has registered with this account service yet. Whoever runs one \
     registers it, and it appears here.";

/// The line above the rows once there are some.
const PICK_ONE: &str = "Pick a server to play on.";

type ChangedButton = (Changed<Interaction>, With<Button>);

fn spawn_server_list(mut commands: Commands) {
    commands
        .spawn((
            ServerListRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.012, 0.016, 0.024, 0.98)),
            Visibility::Hidden,
            GlobalZIndex(SERVERS_LAYER),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        width: Val::Px(460.0),
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        row_gap: Val::Px(14.0),
                        padding: UiRect::all(Val::Px(32.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.065, 0.075, 0.095)),
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new("CHOOSE A SERVER"),
                        TextFont {
                            font_size: FontSize::Px(28.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                    ));
                    // Empty at first and refilled whenever the list changes. A
                    // container that stays put rather than one that is respawned,
                    // so the panel does not jump under the pointer between reads.
                    panel.spawn((
                        ServerRows,
                        Node {
                            width: Val::Percent(100.0),
                            display: Display::Flex,
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(8.0),
                            ..default()
                        },
                    ));
                    panel.spawn((
                        ServerListStatus,
                        Text::new(LOADING),
                        TextFont {
                            font_size: FontSize::Px(15.0),
                            ..default()
                        },
                        TextColor(Color::srgb(0.62, 0.66, 0.74)),
                        Node {
                            max_width: Val::Percent(100.0),
                            ..default()
                        },
                    ));
                    panel
                        .spawn((
                            RetryButton,
                            Button,
                            Node {
                                width: Val::Percent(100.0),
                                height: Val::Px(44.0),
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::Center,
                                border_radius: BorderRadius::all(Val::Px(4.0)),
                                ..default()
                            },
                            BackgroundColor(BUTTON),
                        ))
                        .with_child((
                            Text::new(RETRY_LABEL),
                            TextFont {
                                font_size: FontSize::Px(18.0),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                            TextShadow::default(),
                        ));
                });
        });
}

/// Whether the server list owns the screen this frame.
///
/// Read by `ui/mod.rs` as well: the pointer belongs to whatever is on top, and a row
/// nobody can click is not a row. It is up whenever there is a list to show and no
/// live session — including after a refusal, which is how a player reads why a server
/// was refused and then picks another one.
pub(super) fn server_list_is_up(
    list: Option<&ServerList>,
    state: Option<&ConnectionState>,
    sign_in: Option<&SignInState>,
) -> bool {
    if list.is_none() || login_is_up(sign_in) {
        return false;
    }
    matches!(
        state,
        Some(
            ConnectionState::Idle
                | ConnectionState::Rejected { .. }
                | ConnectionState::Disconnected
        )
    )
}

fn show_server_list(
    list: Option<Res<ServerList>>,
    state: Option<Res<ConnectionState>>,
    sign_in: Option<Res<SignInState>>,
    mut roots: Query<&mut Visibility, With<ServerListRoot>>,
) {
    let next = if server_list_is_up(list.as_deref(), state.as_deref(), sign_in.as_deref()) {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut roots {
        if *visibility != next {
            *visibility = next;
        }
    }
}

/// Replaces the rows whenever the list changes, and only then.
///
/// `is_changed` rather than every frame, for the reason `refresh_login_text` gives one
/// layer over: rebuilding unconditionally would despawn and respawn every button on
/// every frame, which is a pointer that can never finish a press.
fn rebuild_rows(
    list: Option<Res<ServerList>>,
    containers: Query<Entity, With<ServerRows>>,
    rows: Query<Entity, With<ServerRow>>,
    mut commands: Commands,
) {
    let Some(list) = list else {
        return;
    };
    if !list.is_changed() {
        return;
    }

    for row in &rows {
        commands.entity(row).despawn();
    }

    // Rows only for an answer that has rows. `Loading` and `Unavailable` are the line
    // under them, which `refresh_server_list_text` writes — never an empty list, which
    // would read as "no servers exist".
    let ServerList::Ready(servers) = &*list else {
        return;
    };
    for container in &containers {
        commands.entity(container).with_children(|parent| {
            for server in servers {
                spawn_row(parent, server);
            }
        });
    }
}

fn spawn_row(parent: &mut ChildSpawnerCommands<'_>, server: &ListedServer) {
    // `online` is the account service's "I have heard from this one recently", not a
    // reachability probe, so it is a word beside the name rather than a state that
    // stops a player trying.
    let label = if server.online() {
        format!("{}  -  online", server.display_name())
    } else {
        format!("{}  -  offline", server.display_name())
    };
    let colour = if server.online() {
        ONLINE_LABEL
    } else {
        OFFLINE_LABEL
    };

    parent
        .spawn((
            ServerRow(server.name().to_owned()),
            Button,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(48.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
        ))
        .with_child((
            Text::new(label),
            TextFont {
                font_size: FontSize::Px(19.0),
                ..default()
            },
            TextColor(colour),
            TextShadow::default(),
        ));
}

/// A row asks to join the server it names. It never opens a socket.
fn row_action(
    mut rows: Query<(&Interaction, &ServerRow, &mut BackgroundColor), ChangedButton>,
    mut requests: MessageWriter<ConnectRequest>,
) {
    for (interaction, row, mut colour) in &mut rows {
        colour.0 = button_colour(interaction);
        if *interaction == Interaction::Pressed {
            requests.write(ConnectRequest {
                name: row.0.clone(),
            });
        }
    }
}

/// The retry asks for the list again. It never reads one.
fn retry_action(
    mut buttons: Query<(&Interaction, &mut BackgroundColor), (ChangedButton, With<RetryButton>)>,
    mut requests: MessageWriter<RefreshServerList>,
) {
    for (interaction, mut colour) in &mut buttons {
        colour.0 = button_colour(interaction);
        if *interaction == Interaction::Pressed {
            requests.write(RefreshServerList);
        }
    }
}

/// Writes the line under the rows, and only when something moved.
fn refresh_server_list_text(
    list: Option<Res<ServerList>>,
    state: Option<Res<ConnectionState>>,
    mut lines: Query<&mut Text, With<ServerListStatus>>,
) {
    let Some(list) = list else {
        return;
    };
    let moved = list.is_changed() || state.as_ref().is_some_and(|state| state.is_changed());
    if !moved {
        return;
    }

    let line = describe(&list, state.as_deref());
    for mut text in &mut lines {
        if text.0 != line {
            *text = Text::new(line.clone());
        }
    }
}

/// What the line under the rows says.
///
/// **A refusal outranks the list**, and that is the ordering this screen exists to get
/// right: a player who was just refused a server is looking for why, and the list
/// behind them has not changed. `net/tls.rs` writes that sentence — it names the
/// address, both fingerprints and the list as the source of the expectation — and this
/// shows it as it was written, with no bypass added on the way through.
fn describe(list: &ServerList, state: Option<&ConnectionState>) -> String {
    if let Some(ConnectionState::Rejected { reason }) = state {
        return reason.clone();
    }
    if let Some(ConnectionState::Disconnected) = state {
        return "That session ended. Pick a server to play on.".to_owned();
    }
    match list {
        ServerList::Loading => LOADING.to_owned(),
        ServerList::Ready(servers) if servers.is_empty() => NO_SERVERS.to_owned(),
        ServerList::Ready(_) => PICK_ONE.to_owned(),
        // Never an empty list: the account service could not be asked, which is a
        // different thing from it answering that there is nothing to show.
        ServerList::Unavailable(reason) => {
            format!("The login service could not be reached: {reason}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::ListedServer;

    /// Builds the screen headlessly. `MinimalPlugins` has no renderer, so the nodes are
    /// spawned and updated but never drawn — which is exactly the part worth asserting,
    /// and it needs no display.
    fn headless(list: ServerList) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ConnectRequest>()
            .add_message::<RefreshServerList>()
            .insert_resource(list)
            .insert_resource(ConnectionState::Idle)
            .add_plugins(ServerListUiPlugin);
        app.update();
        app
    }

    /// The labels on the rows, **in the order they are drawn in**.
    ///
    /// Read off the container's `Children` rather than from a query, because that is
    /// the order a player sees and a query's is unspecified — the list's own order is
    /// the account service's, and following it is what makes two launches show the
    /// same panel.
    fn row_labels(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        let mut containers = world.query_filtered::<&Children, With<ServerRows>>();
        let rows: Vec<Entity> = containers
            .iter(world)
            .flat_map(|children| children.iter())
            .collect();

        rows.into_iter()
            .filter(|row| world.get::<ServerRow>(*row).is_some())
            .filter_map(|row| {
                let label = world.get::<Children>(row)?.iter().next()?;
                Some(world.get::<Text>(label)?.0.clone())
            })
            .collect()
    }

    /// **A list of two servers, rendered.** One row each, in the account service's
    /// order, each saying whether it has been heard from recently.
    #[test]
    fn two_servers_become_two_rows_a_player_can_read() {
        let mut app = headless(ServerList::Ready(vec![
            ListedServer::for_a_test("midgard", "server.example:7777", true),
            ListedServer::for_a_test("asgard", "other.example:7777", false),
        ]));

        let labels = row_labels(&mut app);
        assert_eq!(labels.len(), 2, "{labels:?}");
        assert!(labels[0].starts_with("midgard"), "{labels:?}");
        assert!(labels[0].contains("online"), "{labels:?}");
        assert!(labels[1].starts_with("asgard"), "{labels:?}");
        assert!(labels[1].contains("offline"), "{labels:?}");
        // The address is what the row is *for*, and it must never be what it says: a
        // screenshot of this panel is a screenshot of somebody's home address
        // otherwise.
        for label in &labels {
            assert!(
                !label.contains("example"),
                "an address reached a row: {label}"
            );
        }
    }

    /// **And one of them connected to.** Pressing a row writes a `ConnectRequest`
    /// naming that server — never an address, which this module never learns.
    #[test]
    fn pressing_a_row_asks_to_join_the_server_it_names() {
        let mut app = headless(ServerList::Ready(vec![
            ListedServer::for_a_test("midgard", "server.example:7777", true),
            ListedServer::for_a_test("asgard", "other.example:7777", true),
        ]));

        let world = app.world_mut();
        let mut query = world.query::<(Entity, &ServerRow)>();
        let asgard = query
            .iter(world)
            .find(|(_, row)| row.0 == "asgard")
            .map(|(entity, _)| entity)
            .expect("a row for the second server");
        *world
            .get_mut::<Interaction>(asgard)
            .expect("a row is a button") = Interaction::Pressed;
        app.update();

        let world = app.world_mut();
        let messages = world.resource::<Messages<ConnectRequest>>();
        let mut cursor = messages.get_cursor();
        let asked: Vec<_> = cursor.read(messages).cloned().collect();
        assert_eq!(
            asked,
            vec![ConnectRequest {
                name: "asgard".to_owned()
            }]
        );
    }

    /// A list that could not be read draws no rows at all — the retry and the line are
    /// what a player gets, and an empty panel of rows would read as "no servers exist".
    #[test]
    fn an_unavailable_list_draws_no_rows() {
        let mut app = headless(ServerList::Unavailable("no route to host".to_owned()));
        assert!(row_labels(&mut app).is_empty());
    }

    /// The retry is always there, and pressing it asks for the list again.
    #[test]
    fn pressing_the_retry_asks_for_the_list_again() {
        let mut app = headless(ServerList::Unavailable("no route to host".to_owned()));

        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<RetryButton>>();
        let button = query.iter(world).next().expect("a retry button");
        *world
            .get_mut::<Interaction>(button)
            .expect("the retry is a button") = Interaction::Pressed;
        app.update();

        let world = app.world_mut();
        let messages = world.resource::<Messages<RefreshServerList>>();
        let mut cursor = messages.get_cursor();
        assert_eq!(cursor.read(messages).count(), 1);
    }

    /// The screen is up over a client that has chosen nothing, and comes down the
    /// moment a session is being opened — a player watching "Connecting..." is not
    /// looking at a list.
    #[test]
    fn the_list_is_up_exactly_while_there_is_no_session() {
        let list = ServerList::Ready(Vec::new());
        let signed_in = SignInState::SignedIn;

        // Driven off `ConnectionState::every` rather than a list of its own, and the
        // expectation is a wildcard-free match: a state added to the enum reaches this
        // sweep by construction, and it cannot be answered for by accident. `Choosing`
        // is why — it existed for two iterations and neither sweep had heard of it.
        for state in ConnectionState::every() {
            let expected = match state {
                // There is no session, so the list is the screen.
                ConnectionState::Idle
                | ConnectionState::Rejected { .. }
                | ConnectionState::Disconnected => true,
                // Something is in progress on a server already chosen. `Choosing` is one
                // of them: the character screen is up and picking a *different server*
                // underneath it is not an offer this client makes.
                ConnectionState::Connecting
                | ConnectionState::Handshaking
                | ConnectionState::Choosing
                | ConnectionState::Connected => false,
            };
            assert_eq!(
                server_list_is_up(Some(&list), Some(&state), Some(&signed_in)),
                expected,
                "{state:?}"
            );
        }
    }

    /// A client with no account service has no list, and this screen is not part of it
    /// — the same shape the login screen keeps.
    #[test]
    fn a_client_with_no_account_service_never_shows_it() {
        assert!(!server_list_is_up(
            None,
            Some(&ConnectionState::Idle),
            Some(&SignInState::SignedIn)
        ));
    }

    /// The login screen is on top of this one. Two overlays claiming the pointer is one
    /// control nobody can press, and the sign-in has to happen first anyway.
    #[test]
    fn the_login_screen_takes_precedence() {
        let list = ServerList::Ready(Vec::new());
        assert!(!server_list_is_up(
            Some(&list),
            Some(&ConnectionState::Idle),
            Some(&SignInState::SignedOut { reason: None })
        ));
        assert!(!server_list_is_up(
            Some(&list),
            Some(&ConnectionState::Idle),
            Some(&SignInState::Waiting)
        ));
    }

    /// **The line a list that could not be read produces is the one this issue is
    /// about.** It says the login service could not be reached and it is shown beside a
    /// retry — never an empty list, which a player reads as "no servers exist".
    #[test]
    fn an_unreachable_list_says_so_rather_than_showing_nothing() {
        let line = describe(
            &ServerList::Unavailable("cannot reach accounts.example:7780".to_owned()),
            Some(&ConnectionState::Idle),
        );
        assert!(line.contains("could not be reached"), "{line}");
        assert!(line.contains("accounts.example:7780"), "{line}");
    }

    /// An account service that answered "none" is a different sentence, because it is a
    /// different fact: there really is nothing to join yet.
    #[test]
    fn an_empty_list_is_a_sentence_rather_than_a_blank_panel() {
        let line = describe(&ServerList::Ready(Vec::new()), Some(&ConnectionState::Idle));
        assert_eq!(line, NO_SERVERS);
        assert!(!line.contains("could not be reached"), "{line}");
    }

    /// A refusal outranks the list, so the sentence `net/tls.rs` wrote is what a player
    /// reads — verbatim, and with no way past it added here.
    #[test]
    fn a_refusal_is_shown_as_it_was_written() {
        let refusal = "refusing to connect to server.example:7777: it presented a different \
                       certificate than the one the server list carries for it.";
        let line = describe(
            &ServerList::Ready(Vec::new()),
            Some(&ConnectionState::Rejected {
                reason: refusal.to_owned(),
            }),
        );
        assert_eq!(line, refusal);
    }
}
