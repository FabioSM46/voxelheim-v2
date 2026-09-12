//! The screen for a session that is over when there is no server list to land on.
//!
//! **A session ending used to be able to leave nothing on screen at all (#1175).** Which
//! screen owns an ended state depends on the launch: the login screen while there is no
//! sign-in, the server list on a launch that has one — and, on the two `--server` launches,
//! which have no list, nothing. The character screen came down with the session, the world
//! was reset with it, and a player whose character screen timed out was left with a black
//! window and no control to press. This is the third owner, and it is up exactly where the
//! other two are not.
//!
//! It says the session ended, or shows a refusal as it was written, and offers two controls
//! centred on the screen. `RECONNECT` is the server list's own button: spawned by
//! `ui/servers.rs`, shown by its one visibility rule and pressed through its one system, so
//! the two screens that offer a way back cannot come to disagree about when there is one.
//! `QUIT` is always there, so a launch with nowhere to go back to still has a way out —
//! there is no list on these launches to return to, and a sign-in has its own screen, which
//! outranks this one.
//!
//! Nothing here decides or dials anything. A press on `RECONNECT` writes one
//! `ReconnectRequest` and `net::reconnect_on_request` owns the rest; nothing writes one
//! without a press, which is the line #184 drew.

use bevy::prelude::*;

use crate::net::{ConnectionState, Rejoining, ServerAddress, ServerList, SignInState};

use super::character::CHARACTER_LAYER;
use super::login::{LOGIN_LAYER, login_is_up};
use super::servers::{reconnect_is_offered, server_list_is_up, spawn_reconnect};
use super::{BUTTON, button_colour};

pub(super) struct SessionEndedUiPlugin;

impl Plugin for SessionEndedUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_session_ended).add_systems(
            Update,
            (show_session_ended, refresh_session_ended_text, quit_action),
        );
    }
}

#[derive(Component)]
struct SessionEndedRoot;

#[derive(Component)]
struct SessionEndedTitle;

/// The line under the title: that the session ended, or the refusal verbatim.
#[derive(Component)]
struct SessionEndedLine;

/// Marks this screen's instance of the server list's [`super::servers::ReconnectButton`].
#[derive(Component)]
struct SessionEndedReconnect;

#[derive(Component)]
struct QuitButton;

/// Above the character screen's 47 and below the login screen's 50.
///
/// Above the character screen deliberately: this screen is up only in a state with no
/// session, and a character screen over one is a form with nothing behind it to answer. The
/// net boundary takes `CharacterChoice` down with every ending, and this ordering is what
/// keeps a missed case a covered screen rather than a dead one.
const SESSION_ENDED_LAYER: i32 = 48;

const _: () = assert!(CHARACTER_LAYER < SESSION_ENDED_LAYER && SESSION_ENDED_LAYER < LOGIN_LAYER);

const ENDED_TITLE: &str = "SESSION ENDED";
const REFUSED_TITLE: &str = "CANNOT PLAY";
const QUIT_LABEL: &str = "QUIT";

/// What the line says when `RECONNECT` is offered beside it.
const GO_BACK: &str = "That session ended. RECONNECT goes back to the same server.";

/// And when it is not: nothing was dialled, so there is no server to name.
const NOWHERE_TO_GO: &str = "That session ended, and there is no server to go back to.";

type ChangedButton = (Changed<Interaction>, With<Button>);

fn spawn_session_ended(mut commands: Commands) {
    commands
        .spawn((
            SessionEndedRoot,
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
            GlobalZIndex(SESSION_ENDED_LAYER),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        width: Val::Px(460.0),
                        max_width: Val::Percent(100.0),
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
                        SessionEndedTitle,
                        Text::new(ENDED_TITLE),
                        TextFont {
                            font_size: FontSize::Px(28.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                    ));
                    panel.spawn((
                        SessionEndedLine,
                        Text::new(GO_BACK),
                        TextFont {
                            font_size: FontSize::Px(15.0),
                            ..default()
                        },
                        TextColor(Color::srgb(0.62, 0.66, 0.74)),
                        TextLayout::default().with_justify(Justify::Center),
                        Node {
                            max_width: Val::Percent(100.0),
                            ..default()
                        },
                    ));
                    // The reason first, then the way straight back, then the way out: the
                    // order a dropped player reads in, and the order the server list keeps.
                    spawn_reconnect(panel, SessionEndedReconnect);
                    panel
                        .spawn((
                            QuitButton,
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
                            Text::new(QUIT_LABEL),
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

/// Whether this screen owns the window this frame.
///
/// **An ended state, and no other screen that owns it.** `Disconnected` and `Rejected` are
/// the two states a session is over in. The login screen outranks everything while there is
/// no sign-in, and the server list is the ending screen of a launch that has one — it says
/// the session ended and carries the same `RECONNECT` beside its rows — so this is up exactly
/// where neither is. Read by `ui/mod.rs` as well, because the pointer belongs to whatever is
/// on top.
///
/// **And not while a rejoin is on its way.** A deliberate leave ends its session in
/// `Disconnected` and dials the character screen back a frame or two later, once the old
/// thread has let go; an ending drawn over that gap would be a screen claiming a return the
/// player had asked for had failed.
pub(super) fn session_ended_is_up(
    state: Option<&ConnectionState>,
    list: Option<&ServerList>,
    sign_in: Option<&SignInState>,
    rejoining: bool,
) -> bool {
    matches!(
        state,
        Some(ConnectionState::Disconnected | ConnectionState::Rejected { .. })
    ) && !rejoining
        && !login_is_up(sign_in)
        && !server_list_is_up(list, state, sign_in)
}

fn show_session_ended(
    state: Option<Res<ConnectionState>>,
    list: Option<Res<ServerList>>,
    sign_in: Option<Res<SignInState>>,
    rejoining: Option<Res<Rejoining>>,
    mut roots: Query<&mut Visibility, With<SessionEndedRoot>>,
) {
    let up = session_ended_is_up(
        state.as_deref(),
        list.as_deref(),
        sign_in.as_deref(),
        rejoining.is_some(),
    );
    let next = if up {
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

/// The title and the line, for one state.
///
/// **A refusal is shown as it was written**, beside the control rather than replaced by it:
/// the reason is the thing a refused player has to be able to read.
fn describe(state: Option<&ConnectionState>, reconnect_offered: bool) -> (&'static str, String) {
    match state {
        Some(ConnectionState::Rejected { reason }) => (REFUSED_TITLE, reason.clone()),
        _ if reconnect_offered => (ENDED_TITLE, GO_BACK.to_owned()),
        _ => (ENDED_TITLE, NOWHERE_TO_GO.to_owned()),
    }
}

fn refresh_session_ended_text(
    state: Option<Res<ConnectionState>>,
    address: Option<Res<ServerAddress>>,
    mut titles: Query<&mut Text, (With<SessionEndedTitle>, Without<SessionEndedLine>)>,
    mut lines: Query<&mut Text, (With<SessionEndedLine>, Without<SessionEndedTitle>)>,
) {
    let (title, line) = describe(
        state.as_deref(),
        reconnect_is_offered(state.as_deref(), address.as_deref()),
    );
    // Written only when they differ, so an idle frame marks no text changed.
    for mut text in &mut titles {
        if text.0 != title {
            text.0 = title.to_owned();
        }
    }
    for mut text in &mut lines {
        if text.0 != line {
            text.0.clone_from(&line);
        }
    }
}

/// Quits the client. The one control this screen draws in every state it is up in.
fn quit_action(
    mut buttons: Query<(&Interaction, &mut BackgroundColor), (ChangedButton, With<QuitButton>)>,
    mut exit: MessageWriter<AppExit>,
) {
    for (interaction, mut colour) in &mut buttons {
        colour.0 = button_colour(interaction);
        if *interaction == Interaction::Pressed {
            exit.write(AppExit::Success);
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;

    use super::super::servers::{ReconnectButton, ServerListUiPlugin};
    use super::*;
    use crate::net::{ConnectRequest, ListedServer, ReconnectRequest, RefreshServerList};

    /// The screen headlessly, beside the server list. The pair is the production one: the
    /// `RECONNECT` on this screen is shown and pressed by the list's systems.
    fn headless(state: ConnectionState) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ConnectRequest>()
            .add_message::<ReconnectRequest>()
            .add_message::<RefreshServerList>()
            .add_message::<AppExit>()
            .insert_resource(state)
            .add_plugins((ServerListUiPlugin, SessionEndedUiPlugin));
        app.update();
        app
    }

    /// A session that ended on a server this client dialled, on a launch with no list: the
    /// `--server` launch the owner reported this from.
    fn after_an_ending(state: ConnectionState) -> App {
        let mut app = headless(state);
        app.insert_resource(ServerAddress("server.example:7777".to_owned()));
        app.update();
        app
    }

    fn root_is_visible(app: &mut App) -> bool {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<SessionEndedRoot>>();
        *query.single(world).expect("one session-ended root") == Visibility::Visible
    }

    fn one<T: Component>(app: &mut App) -> Entity {
        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<T>>();
        query.single(world).expect("exactly one such control")
    }

    fn is_laid_out(app: &App, entity: Entity) -> bool {
        app.world()
            .get::<Node>(entity)
            .is_some_and(|node| node.display != Display::None)
    }

    fn press(app: &mut App, entity: Entity) {
        *app.world_mut()
            .get_mut::<Interaction>(entity)
            .expect("a control is a button") = Interaction::Pressed;
        app.update();
    }

    fn written<M: Message>(app: &App) -> usize {
        let messages = app.world().resource::<Messages<M>>();
        let mut cursor = messages.get_cursor();
        cursor.read(messages).count()
    }

    fn text_of<T: Component>(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<T>>();
        query.single(world).expect("one such text").0.clone()
    }

    /// **The report, headlessly.** A character screen nobody answered ends the session in
    /// `Disconnected`, on a launch with no server list. The list is not what is drawn —
    /// there is none — and before #1175 nothing else was either. Now this screen is up,
    /// centred, with `RECONNECT` on it, and one press asks for exactly one reconnection.
    #[test]
    fn a_character_screen_timeout_with_no_list_leaves_reconnect_centred() {
        let mut app = after_an_ending(ConnectionState::Disconnected);

        assert!(root_is_visible(&mut app), "an ended session drew nothing");
        let root = one::<SessionEndedRoot>(&mut app);
        let node = app.world().get::<Node>(root).expect("the root is a node");
        assert_eq!(node.align_items, AlignItems::Center);
        assert_eq!(node.justify_content, JustifyContent::Center);

        let reconnect = one::<SessionEndedReconnect>(&mut app);
        assert!(app.world().get::<ReconnectButton>(reconnect).is_some());
        assert!(is_laid_out(&app, reconnect), "no RECONNECT was drawn");
        assert_eq!(text_of::<SessionEndedTitle>(&mut app), ENDED_TITLE);
        assert_eq!(text_of::<SessionEndedLine>(&mut app), GO_BACK);

        assert_eq!(written::<ReconnectRequest>(&app), 0, "it dialled unasked");
        press(&mut app, reconnect);
        assert_eq!(written::<ReconnectRequest>(&app), 1);
    }

    /// **And a game that ends mid-play gets the same screen.** An idle timeout, a restart and
    /// a dropped link all move a live session to `Disconnected`, which is the state above.
    #[test]
    fn a_game_that_ends_mid_play_shows_the_same_control() {
        let mut app = after_an_ending(ConnectionState::Connected);
        assert!(
            !root_is_visible(&mut app),
            "an ending drawn over a live game"
        );

        *app.world_mut().resource_mut::<ConnectionState>() = ConnectionState::Disconnected;
        app.update();

        assert!(root_is_visible(&mut app));
        let reconnect = one::<SessionEndedReconnect>(&mut app);
        assert!(is_laid_out(&app, reconnect));
    }

    /// A refusal keeps its own sentence, verbatim, with the way back beside it.
    #[test]
    fn a_refusal_keeps_its_reason_beside_the_control() {
        let reason = "refusing to connect to server.example:7777: it presented a different \
                      certificate than the one this client expected.";
        let mut app = after_an_ending(ConnectionState::Rejected {
            reason: reason.to_owned(),
        });

        assert!(root_is_visible(&mut app));
        assert_eq!(text_of::<SessionEndedTitle>(&mut app), REFUSED_TITLE);
        assert_eq!(text_of::<SessionEndedLine>(&mut app), reason);
        let reconnect = one::<SessionEndedReconnect>(&mut app);
        assert!(is_laid_out(&app, reconnect));
    }

    /// **With nowhere to go back to there is no `RECONNECT`, and still a way out.** No
    /// `ServerAddress` means nothing was dialled, so the control would name no server.
    #[test]
    fn with_nowhere_to_go_back_to_quit_is_the_way_out() {
        let mut app = headless(ConnectionState::Disconnected);

        assert!(root_is_visible(&mut app));
        let reconnect = one::<SessionEndedReconnect>(&mut app);
        assert!(!is_laid_out(&app, reconnect), "a way back to nowhere");
        assert_eq!(text_of::<SessionEndedLine>(&mut app), NOWHERE_TO_GO);

        let quit = one::<QuitButton>(&mut app);
        assert!(is_laid_out(&app, quit));
        press(&mut app, quit);
        assert_eq!(written::<AppExit>(&app), 1);
        assert_eq!(written::<ReconnectRequest>(&app), 0);
    }

    /// A launch with a list keeps the list as its ending screen, which carries the same
    /// `RECONNECT`. Two panels over one ending would be two copies of one control.
    #[test]
    fn a_launch_with_a_list_keeps_the_list_as_its_ending_screen() {
        let mut app = after_an_ending(ConnectionState::Disconnected);
        app.insert_resource(ServerList::Ready(vec![ListedServer::for_a_test(
            "midgard",
            "server.example:7777",
            true,
        )]));
        app.update();

        assert!(!root_is_visible(&mut app));
    }

    /// Nothing is drawn over the frames between a deliberate leave's close and the dial
    /// that takes the player back to the character screen.
    #[test]
    fn a_rejoin_on_its_way_draws_no_ending() {
        let mut app = after_an_ending(ConnectionState::Disconnected);
        app.insert_resource(Rejoining);
        app.update();

        assert!(!root_is_visible(&mut app));
    }

    /// **Nothing dials, and nothing quits, until somebody presses something.**
    #[test]
    fn a_drawn_screen_asks_for_nothing_until_a_press() {
        let mut app = after_an_ending(ConnectionState::Disconnected);
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(written::<ReconnectRequest>(&app), 0);
        assert_eq!(written::<AppExit>(&app), 0);
    }

    /// **No ended state is left without a screen, and none gets two.** Every launch shape
    /// (a list or none), every sign-in and every state: an ended one is owned by exactly one
    /// of the login screen, the server list and this one, and each of the three always draws
    /// a control — the sign-in, the refresh, the quit. The states are matched without a
    /// wildcard, so a state added to the enum has to be answered for here.
    #[test]
    fn every_ended_state_is_owned_by_exactly_one_screen_with_a_control() {
        let list = ServerList::Ready(Vec::new());
        let sign_ins = [
            None,
            Some(SignInState::SignedOut { reason: None }),
            Some(SignInState::Waiting),
            Some(SignInState::SignedIn),
        ];

        for state in ConnectionState::every() {
            let ended = match state {
                ConnectionState::Rejected { .. } | ConnectionState::Disconnected => true,
                ConnectionState::Idle
                | ConnectionState::Connecting
                | ConnectionState::Handshaking
                | ConnectionState::Choosing
                | ConnectionState::Connected
                | ConnectionState::Leaving { .. } => false,
            };
            for list in [None, Some(&list)] {
                for sign_in in &sign_ins {
                    let this = session_ended_is_up(Some(&state), list, sign_in.as_ref(), false);
                    let owners = [
                        login_is_up(sign_in.as_ref()),
                        server_list_is_up(list, Some(&state), sign_in.as_ref()),
                        this,
                    ];
                    let case = format!("{state:?}, list {}, {sign_in:?}", list.is_some());
                    if ended {
                        assert_eq!(
                            owners.iter().filter(|up| **up).count(),
                            1,
                            "{case}: {owners:?}"
                        );
                    } else {
                        assert!(!this, "{case}: an ending drawn with no ending");
                    }
                    assert!(
                        !session_ended_is_up(Some(&state), list, sign_in.as_ref(), true),
                        "{case}: an ending drawn over a rejoin"
                    );
                }
            }
        }
    }

    fn any_overlay_is_up(overlays: super::super::Overlays<'_>) -> bool {
        overlays.any_is_up()
    }

    /// **The screen owns the pointer while it is up**, through the same question every other
    /// full-screen overlay answers in `ui/mod.rs` — a button under a captured, invisible
    /// cursor is a button nobody can press.
    #[test]
    fn the_screen_takes_the_pointer_while_it_is_up() {
        let mut world = World::new();
        world.insert_resource(ConnectionState::Disconnected);
        assert!(
            world
                .run_system_once(any_overlay_is_up)
                .expect("the overlays are readable"),
            "an ended session left the pointer to the world"
        );

        world.insert_resource(ConnectionState::Connected);
        assert!(
            !world
                .run_system_once(any_overlay_is_up)
                .expect("the overlays are readable")
        );
    }
}
