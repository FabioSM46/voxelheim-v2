//! The dungeon-entry consent dialog.
//!
//! **It shows the server's offer and asks; it decides nothing.** Every number on it comes
//! out of `SessionBinding` — how many boss encounters this run has already lost, out of
//! how many, and the second the run resets — and none of it is computed here. Pressing a
//! button writes an [`EntryOfferAnswer`] naming the offer on screen;
//! `player::instance_entry` is what spends the offer and puts an intent on the wire, and
//! the server re-decides the crossing from scratch when it arrives.
//!
//! **Deliberately not the shared yes/no prompt, and deliberately not the sessions
//! window.** `ui::prompt` is a title and two buttons, which is the whole of its
//! abstraction and the reason it is reusable; this dialog is three stated facts and a
//! consequence, and putting them into a title would make the widget the first caller that
//! needed them. It is not the read-only list of runs a character already owes either
//! (#979): that window reports, and this one asks — so this one is modal, sits in the
//! middle of the screen, owns the keyboard, and cannot be opened by a key.

use bevy::prelude::*;

use super::{BUTTON, button_colour};
use crate::player::{ApplyInputMode, EntryOffer, EntryOfferAnswer, InputMode};

/// Wider than the shared confirmation's 360, because this one states facts rather than
/// asking a one-line question, and a wrapped reset stamp reads as an error.
const WIDTH: f32 = 460.0;

/// The one accent on the dialog, on the number the prompt exists to disclose.
const PROGRESS: Color = Color::srgb(1.0, 0.72, 0.25);

#[derive(Component)]
struct EntryOfferRoot;

#[derive(Component, Debug, Clone, Copy)]
struct EntryOfferButton(bool);

type ChangedEntryButtons<'w, 's> = Query<
    'w,
    's,
    (
        &'static Interaction,
        &'static EntryOfferButton,
        &'static mut BackgroundColor,
    ),
    (Changed<Interaction>, With<Button>),
>;

pub(super) struct EntryOfferUiPlugin;

impl Plugin for EntryOfferUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EntryOffer>()
            .init_resource::<InputMode>()
            .add_message::<EntryOfferAnswer>()
            .add_systems(Startup, spawn_dialog)
            .add_systems(
                Update,
                (rebuild_dialog, click_dialog, show_dialog)
                    .chain()
                    .before(ApplyInputMode),
            );
    }
}

fn spawn_dialog(mut commands: Commands) {
    commands.spawn((
        EntryOfferRoot,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(WIDTH),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(12.0),
            padding: UiRect::all(Val::Px(18.0)),
            ..default()
        },
        UiTransform::from_translation(Val2::percent(-50.0, -50.0)),
        // The loot, vendor, player-trade and confirmation frame and ink, exactly. A
        // dialog that invented its own would read as a different application.
        BackgroundColor(Color::srgba(0.025, 0.03, 0.04, 0.96)),
        GlobalZIndex(30),
        Visibility::Hidden,
    ));
}

fn rebuild_dialog(
    offer: Res<EntryOffer>,
    roots: Query<Entity, With<EntryOfferRoot>>,
    mut commands: Commands,
) {
    if !offer.is_changed() {
        return;
    }
    for root in &roots {
        commands.entity(root).despawn_related::<Children>();
        let Some(current) = offer.current() else {
            continue;
        };
        let terms = current.terms;
        commands.entity(root).with_children(|root| {
            line(
                root,
                "This dungeon has a run already under way",
                21.0,
                WHITE,
            );
            line(
                root,
                &format!(
                    "Bosses defeated: {} of {}",
                    terms.bosses_defeated, terms.bosses_total
                ),
                19.0,
                PROGRESS,
            );
            line(
                root,
                &format!("The run resets at {}", utc_stamp(terms.resets_at_unix)),
                16.0,
                MUTED,
            );
            line(
                root,
                "Entering binds you to this run until it resets. Staying outside binds \
                 you to nothing.",
                16.0,
                MUTED,
            );
            root.spawn(Node {
                width: Val::Percent(100.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::FlexEnd,
                column_gap: Val::Px(8.0),
                ..default()
            })
            .with_children(|buttons| {
                spawn_button(buttons, "Stay outside", false);
                spawn_button(buttons, "Enter and bind", true);
            });
        });
    }
}

const WHITE: Color = Color::WHITE;
const MUTED: Color = Color::srgb(0.72, 0.75, 0.80);

fn line(root: &mut ChildSpawnerCommands<'_>, text: &str, size: f32, colour: Color) {
    root.spawn((
        Text::new(text.to_owned()),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(colour),
    ));
}

fn spawn_button(buttons: &mut ChildSpawnerCommands<'_>, label: &str, accept: bool) {
    buttons
        .spawn((
            EntryOfferButton(accept),
            Button,
            Node {
                padding: UiRect::axes(Val::Px(14.0), Val::Px(7.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
        ))
        .with_children(|button| {
            button.spawn((
                Text::new(label.to_owned()),
                TextFont {
                    font_size: FontSize::Px(16.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

/// Turns one press into one answer naming the offer that is on screen.
///
/// **The id is read here rather than assumed downstream.** A superseding offer can arrive
/// in the same frame as this press, and an answer that named nothing would be applied to
/// whichever offer was pending when it was read — which is a player accepting terms they
/// never saw.
fn click_dialog(
    mut buttons: ChangedEntryButtons<'_, '_>,
    offer: Res<EntryOffer>,
    mut answers: MessageWriter<EntryOfferAnswer>,
) {
    for (interaction, button, mut colour) in &mut buttons {
        *colour = button_colour(interaction).into();
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some(current) = offer.current() else {
            continue;
        };
        answers.write(EntryOfferAnswer {
            offer_id: current.offer_id,
            accept: button.0,
        });
    }
}

fn show_dialog(
    offer: Res<EntryOffer>,
    mode: Res<InputMode>,
    mut roots: Query<&mut Visibility, With<EntryOfferRoot>>,
) {
    let visible = *mode == InputMode::EntryOffer && offer.current().is_some();
    for mut visibility in &mut roots {
        let next = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != next {
            *visibility = next;
        }
    }
}

/// The server's reset second as a readable UTC calendar time.
///
/// **UTC and labelled as such, rather than a countdown.** A countdown would be this
/// client subtracting the server's second from its own clock and putting the difference
/// in front of a player as a fact — which is a lockout computed locally, and wrong by
/// exactly however far the two clocks have drifted. A stamp states what the server said
/// and nothing else; the zone is named because the reset follows the *server's* calendar,
/// and a client cannot know whose midnight that is.
///
/// Proleptic Gregorian, Howard Hinnant's `civil_from_days` — the inverse of the
/// `days_from_civil` `net::json` parses timestamps with, so the two agree by construction
/// rather than by luck.
fn utc_stamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
    )
}

/// The civil date `days` after 1970-01-01, proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    // March-based years, so the leap day lands at the end and needs no special case.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{BlockCoord, InstanceEntryOffer, SessionBinding};

    fn offer(offer_id: u64, defeated: u8, total: u8, resets_at_unix: i64) -> InstanceEntryOffer {
        InstanceEntryOffer {
            offer_id,
            terms: SessionBinding {
                arch: BlockCoord {
                    x: -96,
                    y: 61,
                    z: 704,
                },
                bosses_defeated: defeated,
                bosses_total: total,
                resets_at_unix,
            },
        }
    }

    fn app_showing(offer: InstanceEntryOffer) -> App {
        let mut app = App::new();
        app.add_plugins(EntryOfferUiPlugin);
        app.world_mut()
            .resource_mut::<EntryOffer>()
            .open_for_test(offer);
        *app.world_mut().resource_mut::<InputMode>() = InputMode::EntryOffer;
        app.update();
        app
    }

    fn lines(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        world
            .query::<&Text>()
            .iter(world)
            .map(|text| text.0.clone())
            .collect()
    }

    /// The three facts the contract discloses, and the consequence of each answer.
    #[test]
    fn the_dialog_states_the_progress_the_reset_and_what_each_answer_costs() {
        let mut app = app_showing(offer(7, 1, 2, 1_800_000_000));
        let drawn = lines(&mut app);

        assert!(drawn.iter().any(|line| line == "Bosses defeated: 1 of 2"));
        assert!(
            drawn
                .iter()
                .any(|line| line == "The run resets at 2027-01-15 08:00 UTC")
        );
        assert!(
            drawn
                .iter()
                .any(|line| line.contains("binds you to this run"))
        );
        assert!(drawn.iter().any(|line| line == "Enter and bind"));
        assert!(drawn.iter().any(|line| line == "Stay outside"));

        let world = app.world_mut();
        let root = world
            .query_filtered::<(&GlobalZIndex, &Visibility), With<EntryOfferRoot>>()
            .single(world)
            .expect("one dialog root");
        assert_eq!(*root.0, GlobalZIndex(30));
        assert_eq!(*root.1, Visibility::Visible);

        let answers: Vec<bool> = world
            .query::<&EntryOfferButton>()
            .iter(world)
            .map(|button| button.0)
            .collect();
        assert_eq!(answers, [false, true]);
    }

    /// The numbers are the server's, whatever they are. Nothing here counts corpses, and
    /// a run nobody has touched still reports a denominator.
    #[test]
    fn the_progress_is_the_servers_pair_and_never_a_recount() {
        for (defeated, total) in [(0, 1), (2, 3), (u8::MAX, u8::MAX)] {
            let mut app = app_showing(offer(7, defeated, total, 1));
            assert!(
                lines(&mut app)
                    .iter()
                    .any(|line| line == &format!("Bosses defeated: {defeated} of {total}"))
            );
        }
    }

    /// Hidden the moment the offer is gone, whatever the mode says, and hidden in every
    /// mode that is not this dialog's.
    #[test]
    fn the_dialog_cannot_outlive_the_offer_or_leave_its_own_mode() {
        let mut app = app_showing(offer(7, 1, 2, 1));
        app.world_mut()
            .resource_mut::<EntryOffer>()
            .clear_for_test();
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Hidden);

        let mut app = app_showing(offer(7, 1, 2, 1));
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert_eq!(visibility(&mut app), Visibility::Hidden);
    }

    fn visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        *world
            .query_filtered::<&Visibility, With<EntryOfferRoot>>()
            .single(world)
            .expect("one dialog root")
    }

    /// Every press names the offer that was on screen when it happened.
    #[test]
    fn a_press_answers_the_offer_it_was_drawn_for() {
        for accept in [true, false] {
            let mut app = app_showing(offer(41, 1, 2, 1));
            let button = {
                let world = app.world_mut();
                world
                    .query_filtered::<(Entity, &EntryOfferButton), With<Button>>()
                    .iter(world)
                    .find(|(_, button)| button.0 == accept)
                    .map(|(entity, _)| entity)
                    .expect("both answers are drawn")
            };
            *app.world_mut()
                .get_mut::<Interaction>(button)
                .expect("a button is interactive") = Interaction::Pressed;
            app.update();

            assert_eq!(
                app.world_mut()
                    .resource_mut::<Messages<EntryOfferAnswer>>()
                    .drain()
                    .collect::<Vec<_>>(),
                vec![EntryOfferAnswer {
                    offer_id: 41,
                    accept,
                }]
            );
        }
    }

    /// The awkward dates: the epoch, a leap day, the century that is not a leap year, and
    /// the second `net::json` reads back from the account service.
    #[test]
    fn the_stamp_is_the_calendar_the_ticket_parser_already_uses() {
        for (seconds, want) in [
            (1, "1970-01-01 00:00 UTC"),
            (86_399, "1970-01-01 23:59 UTC"),
            (951_825_600, "2000-02-29 12:00 UTC"),
            (1_787_306_594, "2026-08-21 10:03 UTC"),
            (4_107_542_400, "2100-03-01 00:00 UTC"),
        ] {
            assert_eq!(utc_stamp(seconds), want);
        }
    }
}
