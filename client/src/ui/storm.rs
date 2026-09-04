//! The Fimbulvetr warning as presentation state.
//!
//! A [`StormWarning`] is already the server's decision: this module keeps the newest one,
//! publishes its milestone sentence to chat, and lets the compass subtract
//! elapsed wall time from the seconds the server stated. It never infers a storm from the
//! weather snapshot and never writes weather back out of this resource.

use std::time::Instant;

use bevy::prelude::*;

use crate::net::{ConnectionState, DrainNetwork, Session, StormInbox, StormPhase, StormWarning};

use super::{PlayerMessage, PlayerMessageKind, PublishPlayerMessages};

/// The last statement received about the Fimbulvetr.
///
/// `received_at` anchors presentation only. The phase and seconds remain exactly the
/// server's last values; no local timer advances either field.
#[derive(Resource, Debug, Default)]
pub(super) struct Storm {
    last: Option<ReceivedWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReceivedWarning {
    warning: StormWarning,
    received_at: Instant,
}

impl Storm {
    pub(super) fn receive(&mut self, warning: StormWarning, received_at: Instant) {
        self.last = Some(ReceivedWarning {
            warning,
            received_at,
        });
    }

    fn is_clear(&self) -> bool {
        self.last.is_none()
    }

    fn clear(&mut self) {
        self.last = None;
    }

    /// What the compass reads at `now`, or no line when this phase has no countdown.
    pub(super) fn countdown_at(&self, now: Instant) -> Option<String> {
        let received = self.last?;
        countdown_text(received.warning, received.received_at, now)
    }
}

/// Owns the presentation resource and consumes the network inbox.
pub(super) struct StormUiPlugin;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct IngestStorm;

impl Plugin for StormUiPlugin {
    fn build(&self, app: &mut App) {
        // Each producer owns these in the game. Initialising them here keeps the warning
        // path headlessly testable on its own, without a socket or the rest of the UI.
        app.init_resource::<Storm>()
            .init_resource::<StormInbox>()
            .add_message::<PlayerMessage>()
            .add_systems(
                Update,
                ingest_storm_warnings
                    .in_set(IngestStorm)
                    .in_set(PublishPlayerMessages)
                    .after(DrainNetwork),
            );
    }
}

/// Reads every warning once, keeps the newest as state, and publishes each sentence once.
///
/// Chat is written immediately and exactly once for every milestone sentence.
#[derive(bevy::ecs::system::SystemParam)]
struct StormPresentation<'w> {
    state: Option<Res<'w, ConnectionState>>,
    session: Option<Res<'w, Session>>,
    inbox: ResMut<'w, StormInbox>,
    storm: ResMut<'w, Storm>,
    messages: MessageWriter<'w, PlayerMessage>,
}

fn ingest_storm_warnings(mut presentation: StormPresentation<'_>) {
    let connected = presentation.state.as_deref().is_some_and(|state| {
        matches!(
            *state,
            ConnectionState::Connected | ConnectionState::Leaving { .. }
        )
    }) && presentation.session.is_some();

    if !connected {
        if !presentation.inbox.is_empty() {
            drop(presentation.inbox.take());
        }
        if !presentation.storm.is_clear() {
            presentation.storm.clear();
        }
        return;
    }

    if !presentation.inbox.is_empty() {
        for (warning, received_at) in presentation.inbox.take() {
            if let Some(line) = milestone_text(warning) {
                presentation
                    .messages
                    .write(PlayerMessage::new(PlayerMessageKind::Server, line));
            }
            presentation.storm.receive(warning, received_at);
        }
    }
}

/// The milestone sentence carried by one warning.
///
/// The abbreviated one-minute and ten-second lines deliberately begin with three ASCII
/// dots: the source issue uses an ellipsis there, but the embedded fallback font contains
/// only the 95 printable ASCII glyphs. A raging warning needs no second announcement; its
/// remaining time is the persistent line under the compass.
fn milestone_text(warning: StormWarning) -> Option<String> {
    match warning.phase {
        StormPhase::Approaching => Some(match warning.seconds_until {
            600 => "The Fimbulvetr comes in 10 minutes".to_owned(),
            60 => "...in 1 minute".to_owned(),
            10 => "...in 10 seconds".to_owned(),
            seconds if seconds % 60 == 0 => {
                let minutes = seconds / 60;
                let unit = if minutes == 1 { "minute" } else { "minutes" };
                format!("The Fimbulvetr comes in {minutes} {unit}")
            }
            seconds => {
                let unit = if seconds == 1 { "second" } else { "seconds" };
                format!("The Fimbulvetr comes in {seconds} {unit}")
            }
        }),
        StormPhase::Raging => None,
        StormPhase::Passed => Some("The Fimbulvetr has passed".to_owned()),
    }
}

fn countdown_text(warning: StormWarning, received_at: Instant, now: Instant) -> Option<String> {
    let elapsed = now.saturating_duration_since(received_at).as_secs();
    let elapsed = u32::try_from(elapsed).unwrap_or(u32::MAX);
    let remaining = warning.seconds_until.saturating_sub(elapsed);

    match warning.phase {
        StormPhase::Approaching if remaining <= 60 => Some(format_countdown(remaining)),
        StormPhase::Raging => Some(format_countdown(remaining)),
        StormPhase::Approaching | StormPhase::Passed => None,
    }
}

fn format_countdown(seconds: u32) -> String {
    format!("Fimbulvetr | {}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, SessionParams};
    use std::time::Duration;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 5,
            hotbar_slots: 4,
            equipment_slots: 1,
            player_token: ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    fn warning(phase: StormPhase, seconds_until: u32) -> StormWarning {
        StormWarning {
            phase,
            seconds_until,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .insert_resource(ConnectionState::Connected)
            .add_plugins(StormUiPlugin);
        app
    }

    fn deliver(app: &mut App, warning: StormWarning, at: Instant) {
        app.world_mut()
            .resource_mut::<StormInbox>()
            .push(warning, at);
        app.update();
    }

    #[test]
    fn every_phase_has_the_milestone_sentence_it_owns() {
        assert_eq!(
            milestone_text(warning(StormPhase::Approaching, 600)).as_deref(),
            Some("The Fimbulvetr comes in 10 minutes")
        );
        assert_eq!(
            milestone_text(warning(StormPhase::Approaching, 60)).as_deref(),
            Some("...in 1 minute")
        );
        assert_eq!(
            milestone_text(warning(StormPhase::Approaching, 10)).as_deref(),
            Some("...in 10 seconds")
        );
        assert_eq!(milestone_text(warning(StormPhase::Raging, 300)), None);
        assert_eq!(
            milestone_text(warning(StormPhase::Passed, 0)).as_deref(),
            Some("The Fimbulvetr has passed")
        );
    }

    #[test]
    fn milestones_emit_exactly_one_server_message() {
        let at = Instant::now();
        for (warning, expected) in [
            (
                warning(StormPhase::Approaching, 600),
                Some("The Fimbulvetr comes in 10 minutes"),
            ),
            (warning(StormPhase::Raging, 299), None),
            (
                warning(StormPhase::Passed, 0),
                Some("The Fimbulvetr has passed"),
            ),
        ] {
            let mut app = app();
            deliver(&mut app, warning, at);
            let messages = app.world().resource::<Messages<PlayerMessage>>();
            let mut cursor = messages.get_cursor();
            let actual: Vec<PlayerMessage> = cursor.read(messages).cloned().collect();
            assert_eq!(
                actual,
                expected
                    .map(|line| PlayerMessage::new(PlayerMessageKind::Server, line))
                    .into_iter()
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn countdowns_extrapolate_only_from_the_received_warning() {
        let at = Instant::now();
        assert_eq!(
            countdown_text(warning(StormPhase::Approaching, 60), at, at),
            Some("Fimbulvetr | 1:00".to_owned())
        );
        assert_eq!(
            countdown_text(
                warning(StormPhase::Approaching, 60),
                at,
                at + Duration::from_secs(18)
            ),
            Some("Fimbulvetr | 0:42".to_owned())
        );
        assert_eq!(
            countdown_text(warning(StormPhase::Raging, 299), at, at),
            Some("Fimbulvetr | 4:59".to_owned())
        );
        assert_eq!(countdown_text(warning(StormPhase::Passed, 0), at, at), None);
        assert_eq!(
            countdown_text(warning(StormPhase::Approaching, 600), at, at),
            None
        );
    }

    #[test]
    fn disconnect_clears_the_last_warning() {
        let mut app = app();
        deliver(&mut app, warning(StormPhase::Raging, 299), Instant::now());
        assert!(app.world().resource::<Storm>().last.is_some());

        *app.world_mut().resource_mut::<ConnectionState>() = ConnectionState::Disconnected;
        app.update();
        let storm = app.world().resource::<Storm>();
        assert!(storm.last.is_none());
    }

    #[test]
    fn a_missing_connection_state_is_disconnected_even_with_a_session() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .add_plugins(StormUiPlugin);

        deliver(
            &mut app,
            warning(StormPhase::Approaching, 60),
            Instant::now(),
        );

        assert!(app.world().resource::<Storm>().is_clear());
        assert!(app.world().resource::<StormInbox>().is_empty());
        let messages = app.world().resource::<Messages<PlayerMessage>>();
        let mut cursor = messages.get_cursor();
        assert_eq!(cursor.read(messages).count(), 0);
    }
}
