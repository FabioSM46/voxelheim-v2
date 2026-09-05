//! The listener's half of who they hear: one mute and one volume per speaker, and the roster
//! the Voices panel is drawn from.
//!
//! **This is not a decision about audibility, and the distinction is the whole reason this is
//! a module of its own.** The server owns every position and every roster, and receiving a
//! frame *is* its answer that this speaker may be heard — `audio/heard.rs` says so and must go
//! on saying so. What lives here is the opposite question, asked by the person listening: given
//! that the server has decided somebody may be heard, **how loud are they for me**. A gain is
//! not a claim about the world, and turning one to zero is not the client overruling the
//! server about who is in range.
//!
//! ```text
//!   heard.rs ──▶ Voices::heard      (played, so remembered for 60 s)
//!   heard.rs ◀── Voices::gain       (multiplied in before the sum)
//!   ui/      ◀─▶ mute, volume       (the panel; the HUD hides a muted name)
//! ```
//!
//! **Session-scoped, deliberately, and #853 says why**: the snapshot carries no stable player
//! id, only an entity id that a reconnect changes, so a mute written to a file would come back
//! attached to whoever inherited that number. Nothing here is persisted and nothing here
//! reaches [`crate::settings`].
//!
//! Nothing is written down, for `audio/heard.rs`'s reason: how often somebody spoke, and
//! whether anybody muted them, are facts about a person.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bevy::prelude::*;

/// How long a speaker stays on the Voices panel after they were last heard.
///
/// A minute, which is the acceptance criterion's number and a reasonable one: long enough that
/// a player who has just been talked over can find the person who did it, short enough that the
/// list is the conversation rather than the session.
pub const HEARD_FOR: Duration = Duration::from_secs(60);

/// The most speakers the panel remembers at once.
///
/// **A bound on a length the world chooses**, like every other at this boundary: how many
/// people talk near this player in a minute is not something the client decides, and a list
/// that grew with it would be a panel nobody can read and a map nothing prunes. The oldest
/// entry goes, because the panel is for the person who just spoke.
const MAX_REMEMBERED: usize = 32;

/// The quietest a single speaker may be turned to without muting them.
const MIN_VOICE: u8 = 0;
/// And the loudest. Twice unity: a speaker whose microphone is set too low is the case this
/// exists for, and past double the client is amplifying its own noise floor.
pub const MAX_VOICE: u8 = 200;
/// One press of a per-speaker volume control.
const VOICE_STEP: u8 = 10;
/// What a speaker nobody has adjusted is heard at.
pub const DEFAULT_VOICE: u8 = 100;

/// One speaker's adjustment, as the listener left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Adjustment {
    muted: bool,
    /// Per cent of unity, [`MIN_VOICE`] to [`MAX_VOICE`].
    volume: u8,
}

impl Default for Adjustment {
    fn default() -> Self {
        Self {
            muted: false,
            volume: DEFAULT_VOICE,
        }
    }
}

/// Who has been heard lately, and what the listener has done about each of them.
///
/// **Two lifetimes, and they are separate on purpose** — the same shape `audio/heard.rs`'s
/// `Speaking` and its decoders have, one level out. The *roster* is pruned at [`HEARD_FOR`],
/// because it describes the last minute. An *adjustment* is kept for the session, because a
/// player who mutes somebody has not asked to un-mute them a minute later when they stop
/// talking — which is precisely what pruning the two together would do.
#[derive(Resource, Debug, Default)]
pub struct Voices {
    /// Every speaker heard within [`HEARD_FOR`], in the order they were first heard, so a row
    /// does not move under a pointer that is about to press it.
    heard: Vec<(u64, Instant)>,
    /// What the listener set, for as long as this session lasts.
    adjusted: HashMap<u64, Adjustment>,
}

impl Voices {
    /// Records a speaker as having been heard, which is what puts them on the panel.
    ///
    /// Called from the playback path when a frame of theirs was actually decoded, never when
    /// one merely arrived: a speaker whose every frame failed to decode was not heard.
    pub fn heard(&mut self, entity_id: u64, at: Instant) {
        match self.heard.iter_mut().find(|(held, _)| *held == entity_id) {
            Some((_, when)) => *when = at,
            None => {
                // Bounded before the push, and the **least recently heard** goes — which is
                // not the same as the first inserted, because the arm above refreshes a
                // speaker *in place* to keep the list stable for the panel. Evicting index 0
                // dropped whoever was heard first in the session, who on a busy channel is
                // very likely to be the person talking most. Found by review on #941.
                while self.heard.len() >= MAX_REMEMBERED {
                    let Some(stalest) = self
                        .heard
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, (_, when))| *when)
                        .map(|(index, _)| index)
                    else {
                        break;
                    };
                    self.heard.remove(stalest);
                }
                self.heard.push((entity_id, at));
            }
        }
    }

    /// Drops every roster entry older than [`HEARD_FOR`]. Adjustments are untouched — see the
    /// type's own doc for why the two lifetimes are not one.
    pub fn forget_stale(&mut self, now: Instant) {
        self.heard
            .retain(|(_, at)| now.duration_since(*at) < HEARD_FOR);
    }

    /// Whether this speaker is muted for this listener.
    pub fn muted(&self, entity_id: u64) -> bool {
        self.adjusted.get(&entity_id).is_some_and(|held| held.muted)
    }

    /// What this speaker is turned to, as a percentage of unity.
    pub fn volume(&self, entity_id: u64) -> u8 {
        self.adjusted
            .get(&entity_id)
            .map_or(DEFAULT_VOICE, |held| held.volume)
    }

    /// Whether this speaker reaches the listener's ears at all.
    ///
    /// **Not `!muted`**, and the difference is a real state rather than a quibble: `MIN_VOICE`
    /// is zero, so a speaker turned all the way down is inaudible without being muted. Any
    /// reader asking "is this person being heard" wants this; `muted` answers the narrower
    /// question of which button is in which position. Found by review on #941, where the HUD
    /// was naming a speaker contributing nothing.
    pub fn audible(&self, entity_id: u64) -> bool {
        self.gain(entity_id) > 0.0
    }

    /// The gain this speaker is mixed at: zero while muted, else their volume.
    ///
    /// **The one thing `audio/heard.rs` reads**, and it is a number rather than a question:
    /// that module multiplies, it does not ask whether somebody should be audible.
    pub fn gain(&self, entity_id: u64) -> f32 {
        if self.muted(entity_id) {
            return 0.0;
        }
        f32::from(self.volume(entity_id)) / f32::from(DEFAULT_VOICE)
    }
}

/// The half the Voices panel drives.
///
/// Kept as its own block because the two halves have different callers: `audio/heard.rs` and
/// `ui/voice.rs` read the block above every frame, and `ui/settings.rs` is the only thing that
/// reaches these three.
impl Voices {
    /// Every speaker heard within [`HEARD_FOR`] of `now`, oldest first.
    pub fn recent(&self, now: Instant) -> Vec<u64> {
        self.heard
            .iter()
            .filter(|(_, at)| now.duration_since(*at) < HEARD_FOR)
            .map(|(entity_id, _)| *entity_id)
            .collect()
    }

    /// Mutes this speaker, or un-mutes them. The volume is remembered across both.
    pub fn toggle_mute(&mut self, entity_id: u64) {
        let held = self.adjusted.entry(entity_id).or_default();
        held.muted = !held.muted;
    }

    /// Moves this speaker's volume by `steps` of its own size, stopping at its bounds.
    pub fn adjust_volume(&mut self, entity_id: u64, steps: i32) {
        let held = self.adjusted.entry(entity_id).or_default();
        let moved = i32::from(held.volume)
            .saturating_add(steps.saturating_mul(i32::from(VOICE_STEP)))
            .clamp(i32::from(MIN_VOICE), i32::from(MAX_VOICE));
        held.volume = moved as u8;
    }
}

/// Prunes the roster. Added by [`super::AudioPlugin`] beside the playback systems.
pub(super) fn forget_stale_voices(mut voices: ResMut<Voices>) {
    voices.forget_stale(Instant::now());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The roster is a minute and the adjustments are the session, and pruning them together
    /// is the bug this separation exists to stop.** A player who mutes somebody has not asked
    /// to un-mute them a minute later when that person stops talking — which is exactly what a
    /// single lifetime would do, silently, and only for somebody who had gone quiet.
    #[test]
    fn a_mute_outlives_the_roster_entry_that_carried_it() {
        let mut voices = Voices::default();
        let start = Instant::now();
        voices.heard(7, start);
        voices.toggle_mute(7);
        voices.adjust_volume(7, -3);

        assert_eq!(voices.recent(start), vec![7]);
        assert!(voices.muted(7));
        assert_eq!(voices.volume(7), 70);

        let later = start + HEARD_FOR + Duration::from_secs(1);
        voices.forget_stale(later);
        assert!(
            voices.recent(later).is_empty(),
            "the roster kept somebody nobody has heard for a minute"
        );
        assert!(
            voices.muted(7),
            "the mute was forgotten with the roster row"
        );
        assert_eq!(voices.volume(7), 70, "the volume went with it");

        // And they are back on the panel the moment they speak again, still muted.
        voices.heard(7, later);
        assert_eq!(voices.recent(later), vec![7]);
        assert!(voices.muted(7));
    }

    /// Muting is zero gain, un-muting puts the volume back, and the volume survives the round
    /// trip — a mute that reset the volume would be a second thing the button did.
    #[test]
    fn a_mute_is_zero_gain_and_gives_the_volume_back() {
        let mut voices = Voices::default();
        assert!(
            (voices.gain(7) - 1.0).abs() < f32::EPSILON,
            "a speaker nobody touched is unity"
        );

        voices.adjust_volume(7, 5);
        assert_eq!(voices.volume(7), 150);
        assert!((voices.gain(7) - 1.5).abs() < f32::EPSILON);

        voices.toggle_mute(7);
        assert!(
            voices.gain(7).abs() < f32::EPSILON,
            "a muted speaker was audible"
        );
        assert_eq!(voices.volume(7), 150, "muting moved the volume");

        voices.toggle_mute(7);
        assert!(
            (voices.gain(7) - 1.5).abs() < f32::EPSILON,
            "un-muting did not restore the volume"
        );
    }

    /// Both ends of the volume, and that stepping past either stays there.
    #[test]
    fn a_speaker_volume_stops_at_silence_and_at_double() {
        let mut voices = Voices::default();
        voices.adjust_volume(7, 1_000);
        assert_eq!(voices.volume(7), MAX_VOICE);
        assert!((voices.gain(7) - 2.0).abs() < f32::EPSILON);

        voices.adjust_volume(7, -1_000);
        assert_eq!(voices.volume(7), MIN_VOICE);
        assert!(
            voices.gain(7).abs() < f32::EPSILON,
            "a speaker turned all the way down was still audible"
        );
        assert!(
            !voices.muted(7),
            "turning somebody down to nothing is not the same as muting them"
        );
    }

    /// **The roster is bounded by a number this client chose**, not by how many people happen
    /// to be talking. The oldest goes, because the panel is for whoever just spoke.
    #[test]
    fn the_roster_is_bounded_and_drops_its_oldest() {
        let mut voices = Voices::default();
        let now = Instant::now();
        for entity_id in 0..(MAX_REMEMBERED as u64 + 5) {
            voices.heard(entity_id, now);
        }
        let recent = voices.recent(now);
        assert_eq!(recent.len(), MAX_REMEMBERED);
        assert_eq!(
            recent[0], 5,
            "the newest speakers were dropped, not the oldest"
        );
        assert_eq!(recent[MAX_REMEMBERED - 1], MAX_REMEMBERED as u64 + 4);
    }

    /// **The roster evicts the least recently heard, which is not the first inserted.**
    ///
    /// Refreshing a speaker keeps their position, so the vector is in first-heard order and
    /// `remove(0)` drops whoever spoke first in the session — very likely the person talking
    /// most on a busy channel. The comment claimed "the entry furthest from that is the one a
    /// listener is least likely to be reaching for" and the code did something else. Found by
    /// review on #941.
    #[test]
    fn the_roster_evicts_whoever_has_been_quiet_longest_not_whoever_arrived_first() {
        let mut voices = Voices::default();
        let start = Instant::now();

        // The first speaker, and they never stop talking.
        voices.heard(1, start);
        // Then enough others to fill the roster, each heard once and long ago.
        for entity_id in 2..=(MAX_REMEMBERED as u64) {
            voices.heard(entity_id, start);
        }
        let talking = start + Duration::from_secs(1);
        voices.heard(1, talking);

        // One more speaker arrives, so somebody has to go.
        let arriving = start + Duration::from_secs(2);
        voices.heard(999, arriving);

        let recent = voices.recent(arriving);
        assert!(
            recent.contains(&1),
            "the speaker who is still talking was evicted: {recent:?}"
        );
        assert!(
            !recent.contains(&2),
            "the quietest speaker was kept: {recent:?}"
        );
        assert!(recent.contains(&999));
        assert_eq!(recent.len(), MAX_REMEMBERED);

        // And the order the panel draws is still stable — eviction removes, it does not sort.
        assert_eq!(
            recent[0], 1,
            "the surviving rows were reordered: {recent:?}"
        );
    }

    /// **Turned all the way down is inaudible, and `audible` is what says so.**
    ///
    /// `MIN_VOICE` is zero, so a speaker at the bottom of their own volume contributes nothing
    /// while `muted` is still false. Anything asking "is this person being heard" has to ask
    /// the gain; the HUD asked the button, and named somebody nobody could hear (#941).
    #[test]
    fn a_speaker_turned_all_the_way_down_is_inaudible_without_being_muted() {
        let mut voices = Voices::default();
        assert!(voices.audible(7), "a speaker nobody touched was inaudible");

        voices.adjust_volume(7, -1_000);
        assert_eq!(voices.volume(7), MIN_VOICE);
        assert!(!voices.muted(7), "turning down is not muting");
        assert!(
            !voices.audible(7),
            "a speaker mixed at zero was reported audible"
        );

        voices.adjust_volume(7, 1);
        assert!(voices.audible(7));

        voices.toggle_mute(7);
        assert!(!voices.audible(7), "a muted speaker was reported audible");
    }

    /// Hearing somebody again moves their time and does not add a second row.
    #[test]
    fn hearing_a_speaker_again_refreshes_the_one_row_they_have() {
        let mut voices = Voices::default();
        let start = Instant::now();
        voices.heard(7, start);
        voices.heard(9, start);
        let later = start + HEARD_FOR - Duration::from_secs(1);
        voices.heard(7, later);

        let after = start + HEARD_FOR + Duration::from_millis(1);
        voices.forget_stale(after);
        assert_eq!(
            voices.recent(after),
            vec![7],
            "the refreshed speaker was pruned, or the stale one was kept"
        );
    }
}
