//! The login screen: one control, and a line under it.
//!
//! It exists only when an account service is configured — `SignInState` is
//! inserted by `net::SignInPlugin` and nothing else — so a client launched without
//! one draws nothing here and behaves exactly as it did before this screen
//! existed. `Option<Res<SignInState>>` is what encodes that, the same shape
//! `show_menu` uses for `Session`.
//!
//! **One control, and no way past it.** There is no "not now": a player who
//! declines is a player with no ticket, and the screen that says so is the honest
//! place to leave them. What a refusal changes is the line under the control, not
//! whether the game is still running behind it — nothing here panics and nothing
//! blanks the window.
//!
//! Nothing on this screen decides anything. The control writes a
//! [`SignInRequest`]; the network boundary owns the socket, the browser and the
//! ticket, and this module renders the state it publishes.

use bevy::prelude::*;

use crate::net::{SignInRequest, SignInState};

pub(super) struct LoginPlugin;

impl Plugin for LoginPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_login)
            .add_systems(Update, (show_login, login_action, refresh_login_text));
    }
}

#[derive(Component)]
struct LoginRoot;

/// The one control.
#[derive(Component)]
struct SignInButton;

/// The label inside it, which says what pressing it will do — or that it has
/// already been pressed.
#[derive(Component)]
struct SignInLabel;

/// The line under the control: why there is no sign-in, or what is happening.
#[derive(Component)]
struct LoginStatus;

/// Above the pause menu's 40, because a client that is not signed in has nothing
/// for the pause menu to be about yet.
const LOGIN_LAYER: i32 = 50;

const BUTTON: Color = Color::srgb(0.35, 0.40, 0.85);
const BUTTON_HOVERED: Color = Color::srgb(0.44, 0.49, 0.93);
const BUTTON_PRESSED: Color = Color::srgb(0.28, 0.32, 0.70);
/// While an attempt is running the control is not a control. It is greyed rather
/// than removed, so the screen does not change shape under the pointer.
const BUTTON_WAITING: Color = Color::srgb(0.18, 0.20, 0.26);

const SIGN_IN_LABEL: &str = "SIGN IN WITH DISCORD";
const WAITING_LABEL: &str = "WAITING FOR THE BROWSER";

/// What the line under the control says when nothing has gone wrong yet.
const FIRST_TIME: &str = "Sign in once. The game remembers you after that.";

/// And what it says while a tab is open.
const IN_THE_BROWSER: &str =
    "A tab is open in your browser. Finish signing in there and come back.";

type LoginButtonQuery<'a> = (&'a Interaction, &'a mut BackgroundColor);

fn spawn_login(mut commands: Commands) {
    commands
        .spawn((
            LoginRoot,
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
            GlobalZIndex(LOGIN_LAYER),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        width: Val::Px(420.0),
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        row_gap: Val::Px(16.0),
                        padding: UiRect::all(Val::Px(32.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.065, 0.075, 0.095)),
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new("VOXELHEIM"),
                        TextFont {
                            font_size: FontSize::Px(32.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                    ));
                    panel
                        .spawn((
                            SignInButton,
                            Button,
                            Node {
                                width: Val::Percent(100.0),
                                height: Val::Px(52.0),
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::Center,
                                border_radius: BorderRadius::all(Val::Px(4.0)),
                                ..default()
                            },
                            BackgroundColor(BUTTON),
                        ))
                        .with_child((
                            SignInLabel,
                            Text::new(SIGN_IN_LABEL),
                            TextFont {
                                font_size: FontSize::Px(20.0),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                            TextShadow::default(),
                        ));
                    panel.spawn((
                        LoginStatus,
                        Text::new(FIRST_TIME),
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
                });
        });
}

/// The screen is up exactly while there is an account service and no live ticket.
fn show_login(
    sign_in: Option<Res<SignInState>>,
    mut roots: Query<&mut Visibility, With<LoginRoot>>,
) {
    let next = if login_is_up(sign_in.as_deref()) {
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

/// Whether the login screen owns the screen this frame.
///
/// Read by `ui/mod.rs` as well: the pointer belongs to whatever is on top, and a
/// control nobody can click is not a control.
pub(super) fn login_is_up(sign_in: Option<&SignInState>) -> bool {
    matches!(
        sign_in,
        Some(SignInState::SignedOut { .. } | SignInState::Waiting)
    )
}

/// The one control asks for a sign-in. It never starts one.
fn login_action(
    mut buttons: Query<LoginButtonQuery<'_>, (Changed<Interaction>, With<SignInButton>)>,
    sign_in: Option<Res<SignInState>>,
    mut requests: MessageWriter<SignInRequest>,
) {
    let waiting = matches!(sign_in.as_deref(), Some(SignInState::Waiting));
    for (interaction, mut colour) in &mut buttons {
        if waiting {
            colour.0 = BUTTON_WAITING;
            continue;
        }
        colour.0 = match interaction {
            Interaction::Pressed => BUTTON_PRESSED,
            Interaction::Hovered => BUTTON_HOVERED,
            Interaction::None => BUTTON,
        };
        if *interaction == Interaction::Pressed {
            requests.write(SignInRequest);
        }
    }
}

/// Writes the label and the line under it from the state, and only when it moved.
///
/// `is_changed` rather than every frame: a `Text` written unconditionally marks its
/// component changed for the rest of the session, which is the rule four tests in
/// this client already exist to hold.
fn refresh_login_text(
    sign_in: Option<Res<SignInState>>,
    mut labels: Query<&mut Text, (With<SignInLabel>, Without<LoginStatus>)>,
    mut lines: Query<&mut Text, (With<LoginStatus>, Without<SignInLabel>)>,
) {
    let Some(sign_in) = sign_in else {
        return;
    };
    if !sign_in.is_changed() {
        return;
    }

    let (label, line) = match &*sign_in {
        SignInState::Waiting => (WAITING_LABEL, IN_THE_BROWSER),
        SignInState::SignedOut { reason } => {
            (SIGN_IN_LABEL, reason.as_deref().unwrap_or(FIRST_TIME))
        }
        // Nothing reads either while the screen is hidden, and leaving them as
        // they were means a sign-in that is refused later does not first flash the
        // text of the one that worked.
        SignInState::SignedIn => return,
    };

    for mut text in &mut labels {
        if text.0 != label {
            *text = Text::new(label);
        }
    }
    for mut text in &mut lines {
        if text.0 != line {
            *text = Text::new(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<SignInRequest>()
            .add_plugins(LoginPlugin);
        app
    }

    fn visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<LoginRoot>>();
        *query.iter(world).next().expect("a login root")
    }

    fn text_of<C: Component>(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<C>>();
        query.iter(world).next().expect("a text node").0.clone()
    }

    fn press(app: &mut App) {
        let button = {
            let world = app.world_mut();
            let mut query = world.query_filtered::<Entity, With<SignInButton>>();
            query.iter(world).next().expect("the one control")
        };
        *app.world_mut()
            .entity_mut(button)
            .get_mut::<Interaction>()
            .expect("a button has an interaction") = Interaction::Pressed;
        app.update();
    }

    #[test]
    fn without_an_account_service_there_is_no_login_screen() {
        let mut app = app();
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn a_live_ticket_means_the_screen_never_appears() {
        let mut app = app();
        app.insert_resource(SignInState::SignedIn);
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn signed_out_puts_the_screen_up_with_one_control() {
        let mut app = app();
        app.insert_resource(SignInState::SignedOut { reason: None });
        app.update();

        assert_eq!(visibility(&mut app), Visibility::Visible);
        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<SignInButton>>();
        assert_eq!(
            query.iter(world).count(),
            1,
            "a login screen has one control"
        );
        assert_eq!(text_of::<SignInLabel>(&mut app), SIGN_IN_LABEL);
        assert_eq!(text_of::<LoginStatus>(&mut app), FIRST_TIME);
    }

    #[test]
    fn the_control_asks_and_never_starts_anything_itself() {
        let mut app = app();
        app.insert_resource(SignInState::SignedOut { reason: None });
        app.update();
        press(&mut app);

        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<SignInRequest>>()
                .drain()
                .count(),
            1
        );
    }

    #[test]
    fn a_reason_replaces_the_line_under_the_control() {
        let mut app = app();
        app.insert_resource(SignInState::SignedOut { reason: None });
        app.update();

        *app.world_mut().resource_mut::<SignInState>() = SignInState::SignedOut {
            reason: Some("Discord refused the sign-in.".to_owned()),
        };
        app.update();

        assert_eq!(
            text_of::<LoginStatus>(&mut app),
            "Discord refused the sign-in."
        );
        assert_eq!(text_of::<SignInLabel>(&mut app), SIGN_IN_LABEL);
        assert_eq!(
            visibility(&mut app),
            Visibility::Visible,
            "a refusal leaves the screen up rather than blanking it"
        );
    }

    #[test]
    fn waiting_says_so_and_the_control_stops_asking() {
        let mut app = app();
        app.insert_resource(SignInState::SignedOut { reason: None });
        app.update();
        *app.world_mut().resource_mut::<SignInState>() = SignInState::Waiting;
        app.update();

        assert_eq!(text_of::<SignInLabel>(&mut app), WAITING_LABEL);
        assert_eq!(text_of::<LoginStatus>(&mut app), IN_THE_BROWSER);

        press(&mut app);
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<SignInRequest>>()
                .drain()
                .count(),
            0,
            "a second press must not open a second tab"
        );
    }

    #[test]
    fn signing_in_takes_the_screen_away() {
        let mut app = app();
        app.insert_resource(SignInState::SignedOut { reason: None });
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Visible);

        *app.world_mut().resource_mut::<SignInState>() = SignInState::SignedIn;
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn an_idle_frame_does_not_rewrite_the_text() {
        // `Text` written every frame marks its component changed for the rest of
        // the session. Observed from inside a system, because `App::update` ends
        // with `clear_trackers` and a check from outside is always false.
        //
        // **The probe is registered before any frame runs, and that is not
        // tidiness**: a system's first run compares against a last-run tick of
        // zero, so everything in the world reads as changed to it — a probe added
        // afterwards would report a rewrite on a frame that had none. Its first
        // update is therefore spent, and the *second* is the idle frame under test.
        #[derive(Resource, Default)]
        struct Rewritten(bool);

        let mut app = app();
        app.insert_resource(SignInState::SignedOut { reason: None });
        app.init_resource::<Rewritten>();
        app.add_systems(
            Update,
            (|texts: Query<Ref<'_, Text>>, mut seen: ResMut<Rewritten>| {
                seen.0 = texts.iter().any(|text| text.is_changed());
            })
            .after(refresh_login_text),
        );

        app.update();
        app.update();

        assert!(
            !app.world().resource::<Rewritten>().0,
            "an idle frame rewrote the login text"
        );
    }
}
