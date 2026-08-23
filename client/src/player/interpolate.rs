//! The two most recent snapshots, and where they say an entity is right now.
//!
//! Plain Rust apart from the `Resource` derive: no query, no asset, no window. That is
//! what lets the whole interpolation be asserted against exact positions with no app —
//! the same rule `world/mesher.rs` follows, and for the same reason.
//!
//! ## The client renders the past, and that is what makes this interpolation
//!
//! Snapshots arrive one tick apart, so drawing the newest one the instant it lands would
//! leave nothing between it and the next: the picture would snap forward and then sit
//! still. So the client draws **one snapshot interval behind** the newest snapshot it
//! holds. The weight between the two buffered snapshots is then 0 when the newer one
//! arrives and reaches 1 exactly when its successor is due.
//!
//! The acceptance criterion — *"if snapshots stop arriving the last known position holds
//! rather than extrapolating"* — falls out of that as a clamp rather than as a special
//! case: past 1 there is nothing left to interpolate towards, and the weight stops
//! there.
//!
//! The cost is one tick of latency, 50 ms at the server's default rate, and it is bought
//! deliberately. **Nothing here predicts.** The client does not correct, rewind, or run
//! its own physics; it draws the answers it was given, slightly late, in between.

use std::f32::consts::{PI, TAU};
use std::time::{Duration, Instant};

use bevy::prelude::*;

use crate::net::{
    EntityState, ItemDropState, MobAction, MobKind, MobState, Snapshot, StructureState,
};

/// Where an entity should be drawn now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interpolated {
    pub pos: Vec3,
    pub yaw: f32,
}

/// Where one authoritative item drop should be drawn now.
///
/// Position is the only interpolated field. The item is the newest snapshot's
/// value; count deliberately does not enter the render model because the client
/// draws neither a number nor any inferred inventory outcome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterpolatedDrop {
    pub pos: Vec3,
    pub item_id: u16,
}

/// Where one mob should be drawn now, and what it is doing.
///
/// Position and yaw interpolate; everything else is the newest snapshot's value and
/// nothing else. Kind, health and action are discrete facts about a creature rather than
/// points on a segment — blending two actions would draw a pose the server never chose,
/// and blending two healths would invent a moment the draugr was never at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterpolatedMob {
    pub pos: Vec3,
    pub yaw: f32,
    pub kind: MobKind,
    pub health: u16,
    pub max_health: u16,
    pub action: MobAction,
}

/// One snapshot, with the moment it reached this process.
#[derive(Debug, Clone)]
struct Received {
    snapshot: Snapshot,
    at: Instant,
}

/// The two most recent snapshots a session has received.
///
/// Two, not a deep queue: interpolating between the newest pair is what the contract
/// asks for, and holding more would only let the client fall further behind the server
/// it is meant to be following.
#[derive(Resource, Debug, Default)]
pub struct SnapshotBuffer {
    previous: Option<Received>,
    latest: Option<Received>,
}

impl SnapshotBuffer {
    /// Takes a snapshot, keeping it and the one before it.
    ///
    /// Returns `false` for a snapshot that is not newer than the newest one held. Server
    /// ticks are monotonic per session, so anything else is a duplicate — and applying
    /// one would move the interpolation backwards through positions already drawn.
    pub fn accept(&mut self, snapshot: Snapshot, at: Instant) -> bool {
        if let Some(latest) = &self.latest
            && !is_newer(snapshot.server_tick, latest.snapshot.server_tick)
        {
            return false;
        }

        self.previous = self.latest.take();
        self.latest = Some(Received { snapshot, at });
        true
    }

    /// The newest server tick held, if any. Diagnostics only.
    pub fn latest_tick(&self) -> Option<u32> {
        self.latest.as_ref().map(|held| held.snapshot.server_tick)
    }

    /// The velocity the newest snapshot gave an entity, if it named it.
    ///
    /// The server's own number, read rather than derived. Differencing two interpolated
    /// positions would look like the same thing and read zero on every frame that landed
    /// inside one tick — which is most of them, at 60 frames against 20 snapshots.
    pub fn velocity_of(&self, entity_id: u64) -> Option<[f32; 3]> {
        self.latest
            .as_ref()?
            .snapshot
            .entities
            .iter()
            .find(|state| state.entity_id == entity_id)
            .map(|state| state.vel)
    }

    /// Whether the newest snapshot says this player is dead.
    ///
    /// The sparse list is the complete authoritative answer for that snapshot. Absence
    /// means alive, including after a respawn; no health value, missing snapshot or local
    /// inference participates in the answer.
    pub fn player_is_dead(&self, entity_id: u64) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|latest| latest.snapshot.dead_players.contains(&entity_id))
    }

    /// Every structure the newest snapshot names, exactly as it named them.
    ///
    /// **Not a sample, and it takes no `now`** — that is the whole point of the method
    /// existing beside the three above rather than joining them. A structure has no
    /// position and no velocity on the wire: it is an anchor cell and a facing, and both
    /// are the same in every snapshot it appears in. There is nothing to blend, so there
    /// is no code path on which one can be blended, which is what keeps a building out of
    /// the entity-motion path by construction rather than by discipline.
    ///
    /// The newest snapshot is the complete existence set, exactly as it is for mobs. A
    /// structure it omits is gone, and this client does not guess why — taken back by its
    /// owner, collapsed under a block somebody broke, and simply out of view all look
    /// identical from here.
    pub fn structures(&self) -> &[StructureState] {
        match &self.latest {
            Some(latest) => &latest.snapshot.structures,
            None => &[],
        }
    }

    /// Where every entity the session can see should be drawn at `now`.
    ///
    /// `interval` is one server tick, derived from `ServerWelcome.tick_rate` — the
    /// server's number, never a constant here.
    ///
    /// The list comes from the **latest** snapshot: an entity that has left the server's
    /// answer has left the world this client draws, whatever the previous snapshot said.
    /// An entity that is only in the latest one is placed there rather than interpolated,
    /// because there is no earlier position to come from — it has just come into view.
    pub fn sample(&self, now: Instant, interval: Duration) -> Vec<(u64, Interpolated)> {
        let Some(latest) = &self.latest else {
            return Vec::new();
        };

        let Some(previous) = &self.previous else {
            // One snapshot is not a segment. Drawing it exactly is the honest answer for
            // the first tick of a session.
            return latest
                .snapshot
                .entities
                .iter()
                .map(|state| (state.entity_id, at_rest(state)))
                .collect();
        };

        let weight = blend(
            previous.snapshot.server_tick,
            latest.snapshot.server_tick,
            latest.at,
            now,
            interval,
        );

        latest
            .snapshot
            .entities
            .iter()
            .map(|state| {
                let from = previous
                    .snapshot
                    .entities
                    .iter()
                    .find(|earlier| earlier.entity_id == state.entity_id);

                let drawn = match from {
                    Some(from) => Interpolated {
                        pos: position(from).lerp(position(state), weight),
                        yaw: lerp_angle(from.yaw, state.yaw, weight),
                    },
                    None => at_rest(state),
                };
                (state.entity_id, drawn)
            })
            .collect()
    }

    /// Where every authoritative item drop should be drawn at `now`.
    ///
    /// This is deliberately the entity interpolation beside it: the newest snapshot
    /// is the complete existence set, a new id starts at its first known position, and
    /// an id present in both snapshots travels over the same tick-derived blend. A drop
    /// omitted by the newest snapshot is therefore absent immediately; the client does
    /// not guess whether it was collected, merged, or timed out.
    pub fn sample_drops(&self, now: Instant, interval: Duration) -> Vec<(u64, InterpolatedDrop)> {
        let Some(latest) = &self.latest else {
            return Vec::new();
        };

        let Some(previous) = &self.previous else {
            return latest
                .snapshot
                .drops
                .iter()
                .map(|state| (state.entity_id, drop_at_rest(state)))
                .collect();
        };

        let weight = blend(
            previous.snapshot.server_tick,
            latest.snapshot.server_tick,
            latest.at,
            now,
            interval,
        );

        latest
            .snapshot
            .drops
            .iter()
            .map(|state| {
                let from = previous
                    .snapshot
                    .drops
                    .iter()
                    .find(|earlier| earlier.entity_id == state.entity_id);
                let mut drawn = drop_at_rest(state);
                if let Some(from) = from {
                    drawn.pos = drop_position(from).lerp(drop_position(state), weight);
                }
                (state.entity_id, drawn)
            })
            .collect()
    }

    /// Where every mob the session can see should be drawn at `now`.
    ///
    /// Deliberately the drop sampling beside it, with one difference that is the whole of
    /// what a mob adds: yaw interpolates too, because a draugr turns to face what it is
    /// hunting and a body that snapped between facings twenty times a second would read
    /// as a stutter rather than as a turn.
    ///
    /// The newest snapshot is the complete existence set. A mob it omits is gone
    /// immediately and this client does not guess why — killed, despawned, or simply
    /// walked out of the view cube all look identical from here, and only one of them is
    /// death.
    pub fn sample_mobs(&self, now: Instant, interval: Duration) -> Vec<(u64, InterpolatedMob)> {
        let Some(latest) = &self.latest else {
            return Vec::new();
        };

        let Some(previous) = &self.previous else {
            return latest
                .snapshot
                .mobs
                .iter()
                .map(|state| (state.entity_id, mob_at_rest(state)))
                .collect();
        };

        let weight = blend(
            previous.snapshot.server_tick,
            latest.snapshot.server_tick,
            latest.at,
            now,
            interval,
        );

        latest
            .snapshot
            .mobs
            .iter()
            .map(|state| {
                let from = previous
                    .snapshot
                    .mobs
                    .iter()
                    .find(|earlier| earlier.entity_id == state.entity_id);

                let mut drawn = mob_at_rest(state);
                if let Some(from) = from {
                    drawn.pos = Vec3::from_array(from.pos).lerp(drawn.pos, weight);
                    drawn.yaw = lerp_angle(from.yaw, state.yaw, weight);
                }
                (state.entity_id, drawn)
            })
            .collect()
    }
}

/// An entity drawn exactly where the snapshot puts it.
fn at_rest(state: &EntityState) -> Interpolated {
    Interpolated {
        pos: position(state),
        yaw: state.yaw,
    }
}

fn position(state: &EntityState) -> Vec3 {
    Vec3::from_array(state.pos)
}

/// A mob drawn exactly where the snapshot puts it.
fn mob_at_rest(state: &MobState) -> InterpolatedMob {
    InterpolatedMob {
        pos: Vec3::from_array(state.pos),
        yaw: state.yaw,
        kind: state.kind,
        health: state.health,
        max_health: state.max_health,
        action: state.action,
    }
}

/// A drop drawn exactly where the snapshot puts it.
fn drop_at_rest(state: &ItemDropState) -> InterpolatedDrop {
    InterpolatedDrop {
        pos: drop_position(state),
        item_id: state.item_id,
    }
}

fn drop_position(state: &ItemDropState) -> Vec3 {
    Vec3::from_array(state.pos)
}

/// How far along the segment between two snapshots to draw, in 0..=1.
///
/// The render time is `now - interval`: one snapshot interval in the past, which is what
/// makes this an interpolation rather than a guess about the future. The pair may be more
/// than one tick apart — a dropped snapshot, or one the server never sent because the
/// session's queue was full — so the span is measured in ticks and the weight moves
/// across it proportionally.
///
/// Clamped at both ends. Below 0 would draw before the older snapshot, which cannot be
/// reached because the render time is behind the newer one by construction; above 1 is
/// what happens when snapshots stop, and clamping there is precisely "hold the last
/// known position rather than extrapolate".
fn blend(
    previous_tick: u32,
    latest_tick: u32,
    latest_at: Instant,
    now: Instant,
    interval: Duration,
) -> f32 {
    // wrapping_sub, because server_tick is a uint32 and a session can outlive it: at
    // 20 Hz it wraps after about seven years, and a plain subtraction would then make the
    // span four billion ticks wide and freeze the interpolation for good.
    let ticks = latest_tick.wrapping_sub(previous_tick).max(1) as f32;
    let one_tick = interval.as_secs_f32();
    let span = one_tick * ticks;
    if span <= 0.0 {
        // Unreachable: `tick_rate >= 1` is a decoder invariant, so an interval is never
        // zero. Answered rather than divided by, because the alternative is a NaN
        // position and this module exists to keep those out of the transforms.
        return 1.0;
    }

    let elapsed = now.saturating_duration_since(latest_at).as_secs_f32();
    ((elapsed + span - one_tick) / span).clamp(0.0, 1.0)
}

/// Interpolates an angle the short way round the circle.
///
/// A plain lerp from just under π to just over -π would spin the body most of the way
/// round instead of a hair across the join. Yaw is a direction, not a number, and the
/// server wraps it into (-π, π] precisely so this is the only case to handle.
fn lerp_angle(from: f32, to: f32, weight: f32) -> f32 {
    let mut delta = (to - from) % TAU;
    if delta > PI {
        delta -= TAU;
    } else if delta < -PI {
        delta += TAU;
    }
    from + delta * weight
}

/// Whether `tick` is later than `newest`, tolerating the uint32 wrap.
///
/// The subtraction read as signed is what puts 0 immediately after 0xFFFFFFFF instead of
/// four billion ticks before it. The server's `newerTick` is the same test, and for the
/// same reason.
fn is_newer(tick: u32, newest: u32) -> bool {
    (tick.wrapping_sub(newest) as i32) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 Hz, the server's default.
    const INTERVAL: Duration = Duration::from_millis(50);

    fn state(entity_id: u64, x: f32, yaw: f32) -> EntityState {
        EntityState {
            entity_id,
            pos: [x, 64.0, 0.0],
            vel: [0.0, 0.0, 0.0],
            yaw,
        }
    }

    fn snapshot(tick: u32, entities: Vec<EntityState>) -> Snapshot {
        Snapshot {
            server_tick: tick,
            entities,
            drops: vec![],
            ..Default::default()
        }
    }

    /// A buffer holding two consecutive snapshots, and the instant the newer arrived.
    fn two_snapshots() -> (SnapshotBuffer, Instant) {
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();

        assert!(buffer.accept(snapshot(1, vec![state(1, 0.0, 0.0)]), start));
        let arrived = start + INTERVAL;
        assert!(buffer.accept(snapshot(2, vec![state(1, 4.0, 0.0)]), arrived));

        (buffer, arrived)
    }

    fn only(sampled: Vec<(u64, Interpolated)>) -> Interpolated {
        assert_eq!(sampled.len(), 1, "expected exactly one entity");
        sampled[0].1
    }

    /// The newest snapshot's structures, verbatim, and no sampling anywhere near them.
    ///
    /// This is the whole of "structures are exempt from the entity-motion path": the
    /// accessor takes no `now` and no interval, so there is no call a caller could make
    /// that would blend one — where a body, a drop and a mob each have a sampler that
    /// would.
    #[test]
    fn the_structures_of_the_newest_snapshot_are_handed_over_unsampled() {
        use crate::net::{BlockCoord, Facing, StructureKind, StructureState};

        let tent = |structure_id: u64, x: i32| StructureState {
            structure_id,
            kind: StructureKind::Tent,
            anchor: BlockCoord { x, y: 63, z: -7 },
            facing: Facing::East,
            owner_entity_id: 7,
        };

        let mut buffer = SnapshotBuffer::default();
        assert!(
            buffer.structures().is_empty(),
            "a buffer with no snapshot has nothing standing in it"
        );

        let start = Instant::now();
        buffer.accept(
            Snapshot {
                server_tick: 1,
                structures: vec![tent(900, 4), tent(901, 9)],
                ..Default::default()
            },
            start,
        );
        assert_eq!(buffer.structures(), [tent(900, 4), tent(901, 9)]);

        // The newest snapshot is the complete existence set: one it omits is gone, and the
        // previous snapshot having named it changes nothing.
        buffer.accept(
            Snapshot {
                server_tick: 2,
                structures: vec![tent(901, 9)],
                ..Default::default()
            },
            start + INTERVAL,
        );
        assert_eq!(buffer.structures(), [tent(901, 9)]);
    }

    #[test]
    fn the_velocity_comes_from_the_newest_snapshot() {
        let mut buffer = SnapshotBuffer::default();
        let now = Instant::now();
        assert_eq!(buffer.velocity_of(1), None);

        buffer.accept(
            snapshot(
                1,
                vec![EntityState {
                    entity_id: 1,
                    pos: [0.0, 64.0, 0.0],
                    vel: [3.0, -1.0, 0.0],
                    yaw: 0.0,
                }],
            ),
            now,
        );

        assert_eq!(buffer.velocity_of(1), Some([3.0, -1.0, 0.0]));
        assert_eq!(buffer.velocity_of(2), None, "an entity nobody sent");
    }

    #[test]
    fn an_empty_buffer_draws_nothing() {
        let buffer = SnapshotBuffer::default();
        assert!(buffer.sample(Instant::now(), INTERVAL).is_empty());
        assert_eq!(buffer.latest_tick(), None);
    }

    #[test]
    fn one_snapshot_is_drawn_where_it_says() {
        // The first tick of a session: there is no segment yet, and the honest answer is
        // the position the server sent rather than a guess either side of it.
        let mut buffer = SnapshotBuffer::default();
        let now = Instant::now();
        buffer.accept(snapshot(7, vec![state(1, 3.5, 0.25)]), now);

        let drawn = only(buffer.sample(now + INTERVAL * 3, INTERVAL));
        assert_eq!(drawn.pos, Vec3::new(3.5, 64.0, 0.0));
        assert_eq!(drawn.yaw, 0.25);
        assert_eq!(buffer.latest_tick(), Some(7));
    }

    #[test]
    fn the_interpolated_position_lands_on_the_segment() {
        // The acceptance criterion, and the whole property: given two snapshots and an
        // elapsed time, the drawn position is a point *between* them — never outside, and
        // never one of them until the ends.
        let (buffer, arrived) = two_snapshots();

        for (elapsed, want_x) in [
            (Duration::ZERO, 0.0),
            (INTERVAL / 4, 1.0),
            (INTERVAL / 2, 2.0),
            (INTERVAL * 3 / 4, 3.0),
            (INTERVAL, 4.0),
        ] {
            let drawn = only(buffer.sample(arrived + elapsed, INTERVAL));
            assert!(
                (drawn.pos.x - want_x).abs() < 1e-4,
                "after {elapsed:?} the entity is drawn at x = {}, want {want_x}",
                drawn.pos.x
            );
            assert!(
                (0.0..=4.0).contains(&drawn.pos.x),
                "x = {} is off the segment between 0 and 4",
                drawn.pos.x
            );
            assert_eq!(drawn.pos.y, 64.0, "the other axes are interpolated too");
        }
    }

    #[test]
    fn the_last_known_position_holds_when_snapshots_stop() {
        // The other half of the criterion. Past the end of the segment there is nothing
        // to interpolate towards, and the client must sit on the last answer it was given
        // rather than carry on in the direction it was heading.
        let (buffer, arrived) = two_snapshots();

        for late in [
            INTERVAL,
            INTERVAL * 2,
            INTERVAL * 100,
            Duration::from_secs(60),
        ] {
            let drawn = only(buffer.sample(arrived + late, INTERVAL));
            assert_eq!(
                drawn.pos,
                Vec3::new(4.0, 64.0, 0.0),
                "{late:?} after the last snapshot the entity had moved on"
            );
        }
    }

    #[test]
    fn a_sample_before_the_newest_snapshot_arrived_stays_on_the_segment() {
        // Cannot happen through the plugin — a frame runs after the drain that fed the
        // buffer — but `Instant`s come from outside this module and the clamp is what
        // keeps an out-of-order one from drawing behind the older snapshot.
        let (buffer, arrived) = two_snapshots();

        let drawn = only(buffer.sample(arrived - INTERVAL * 4, INTERVAL));
        assert_eq!(drawn.pos, Vec3::new(0.0, 64.0, 0.0));
    }

    #[test]
    fn a_gap_between_snapshots_stretches_the_interpolation_over_it() {
        // A snapshot the server dropped for a full queue leaves the pair two ticks apart.
        // The segment then has to be crossed in two intervals, not one, or the entity
        // would arrive early and wait.
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(snapshot(10, vec![state(1, 0.0, 0.0)]), start);
        let arrived = start + INTERVAL * 2;
        buffer.accept(snapshot(12, vec![state(1, 4.0, 0.0)]), arrived);

        // One interval behind the newest, which is halfway along a two-interval segment.
        let midpoint = only(buffer.sample(arrived, INTERVAL));
        assert!(
            (midpoint.pos.x - 2.0).abs() < 1e-4,
            "drawn at x = {}, want the halfway point 2.0",
            midpoint.pos.x
        );

        let end = only(buffer.sample(arrived + INTERVAL, INTERVAL));
        assert!((end.pos.x - 4.0).abs() < 1e-4, "drawn at x = {}", end.pos.x);
    }

    #[test]
    fn a_snapshot_that_is_not_newer_is_refused() {
        let mut buffer = SnapshotBuffer::default();
        let now = Instant::now();

        assert!(buffer.accept(snapshot(5, vec![state(1, 0.0, 0.0)]), now));
        assert!(
            !buffer.accept(snapshot(5, vec![state(1, 99.0, 0.0)]), now),
            "the same tick again is a duplicate"
        );
        assert!(
            !buffer.accept(snapshot(4, vec![state(1, 99.0, 0.0)]), now),
            "an older tick would move the interpolation backwards"
        );
        assert_eq!(buffer.latest_tick(), Some(5));

        let drawn = only(buffer.sample(now, INTERVAL));
        assert_eq!(drawn.pos.x, 0.0, "a refused snapshot was drawn");
    }

    #[test]
    fn the_server_tick_may_wrap() {
        // uint32 at 20 Hz wraps after about seven years. A plain comparison would refuse
        // every snapshot after the wrap and freeze the world for good.
        let mut buffer = SnapshotBuffer::default();
        let now = Instant::now();

        assert!(buffer.accept(snapshot(u32::MAX, vec![state(1, 0.0, 0.0)]), now));
        assert!(
            buffer.accept(snapshot(0, vec![state(1, 4.0, 0.0)]), now + INTERVAL),
            "the tick after 0xFFFFFFFF was refused as stale"
        );

        // And the span across the wrap is one tick, not four billion.
        let drawn = only(buffer.sample(now + INTERVAL * 2, INTERVAL));
        assert!(
            (drawn.pos.x - 4.0).abs() < 1e-4,
            "drawn at x = {}",
            drawn.pos.x
        );
    }

    #[test]
    fn an_entity_that_has_just_come_into_view_is_placed_rather_than_interpolated() {
        // There is no earlier position to come from. Interpolating from a default would
        // slide the newcomer in from the origin, across the whole world.
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(snapshot(1, vec![state(1, 0.0, 0.0)]), start);
        let arrived = start + INTERVAL;
        buffer.accept(
            snapshot(2, vec![state(1, 4.0, 0.0), state(2, 500.0, 1.0)]),
            arrived,
        );

        let sampled = buffer.sample(arrived, INTERVAL);
        let newcomer = sampled
            .iter()
            .find(|(id, _)| *id == 2)
            .expect("the new entity is drawn");
        assert_eq!(newcomer.1.pos, Vec3::new(500.0, 64.0, 0.0));
        assert_eq!(newcomer.1.yaw, 1.0);
    }

    #[test]
    fn an_entity_that_has_left_the_view_is_not_drawn() {
        // The latest snapshot is the whole truth about what this session can see. An
        // entity kept because the previous one mentioned it would be a ghost.
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(
            snapshot(1, vec![state(1, 0.0, 0.0), state(2, 10.0, 0.0)]),
            start,
        );
        buffer.accept(snapshot(2, vec![state(1, 4.0, 0.0)]), start + INTERVAL);

        let sampled = buffer.sample(start + INTERVAL, INTERVAL);
        assert_eq!(sampled.len(), 1);
        assert_eq!(sampled[0].0, 1);
    }

    #[test]
    fn yaw_turns_the_short_way_round() {
        // Across the join at ±π a plain lerp goes almost all the way round the circle. A
        // body that spins 350° instead of turning 10° is the visible symptom.
        assert!((lerp_angle(PI - 0.1, -PI + 0.1, 0.5) - PI).abs() < 1e-4);
        assert!((lerp_angle(-PI + 0.1, PI - 0.1, 0.5) + PI).abs() < 1e-4);

        // And the ordinary case is an ordinary lerp.
        assert!((lerp_angle(0.0, 1.0, 0.25) - 0.25).abs() < 1e-6);
        assert!((lerp_angle(1.0, 1.0, 0.5) - 1.0).abs() < 1e-6);

        // Never further than half a turn, whatever the inputs.
        for from in [-3.0, -1.0, 0.0, 1.0, 3.0] {
            for to in [-3.0, -1.0, 0.0, 1.0, 3.0] {
                let travelled = (lerp_angle(from, to, 1.0) - from).abs();
                assert!(travelled <= PI + 1e-5, "{from} -> {to} turned {travelled}");
            }
        }
    }

    #[test]
    fn yaw_is_interpolated_between_the_snapshots() {
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(snapshot(1, vec![state(1, 0.0, 0.0)]), start);
        let arrived = start + INTERVAL;
        buffer.accept(snapshot(2, vec![state(1, 0.0, 1.0)]), arrived);

        let halfway = only(buffer.sample(arrived + INTERVAL / 2, INTERVAL));
        assert!((halfway.yaw - 0.5).abs() < 1e-4, "yaw = {}", halfway.yaw);
    }

    #[test]
    fn a_one_hertz_server_is_interpolated_over_a_whole_second() {
        // The interval is the server's number. A client that assumed 20 Hz would race to
        // the newest position in 50 ms and then sit still for 950.
        let second = Duration::from_secs(1);
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(snapshot(1, vec![state(1, 0.0, 0.0)]), start);
        let arrived = start + second;
        buffer.accept(snapshot(2, vec![state(1, 4.0, 0.0)]), arrived);

        let halfway = only(buffer.sample(arrived + second / 2, second));
        assert!((halfway.pos.x - 2.0).abs() < 1e-4, "x = {}", halfway.pos.x);
    }
}

#[cfg(test)]
mod mob_tests {
    //! Plain Rust: no app, no Bevy world. The buffer is a value and these are questions
    //! about it.

    use super::*;

    /// One server tick at the rate these tests pretend the session announced.
    const INTERVAL: Duration = Duration::from_millis(50);

    fn mob(entity_id: u64, x: f32, yaw: f32, health: u16, action: MobAction) -> MobState {
        MobState {
            entity_id,
            kind: MobKind::Draugr,
            pos: [x, 64.0, 0.0],
            vel: [0.0; 3],
            yaw,
            health,
            max_health: 60,
            action,
        }
    }

    fn with_mobs(tick: u32, mobs: Vec<MobState>) -> Snapshot {
        Snapshot {
            server_tick: tick,
            mobs,
            ..Default::default()
        }
    }

    /// Position and yaw travel; the discrete facts do not.
    #[test]
    fn a_mob_in_both_snapshots_is_interpolated_between_them() {
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(
            with_mobs(1, vec![mob(900, 0.0, 0.0, 60, MobAction::Chase)]),
            start,
        );
        buffer.accept(
            with_mobs(2, vec![mob(900, 2.0, 1.0, 35, MobAction::Windup)]),
            start + INTERVAL,
        );

        let drawn = buffer.sample_mobs(start + INTERVAL + INTERVAL / 2, INTERVAL);
        assert_eq!(drawn.len(), 1);
        let (entity_id, state) = drawn[0];
        assert_eq!(entity_id, 900);

        assert!(
            (state.pos.x - 1.0).abs() < 1e-4,
            "half way between 0 and 2 is {}",
            state.pos.x
        );
        assert!(
            (state.yaw - 0.5).abs() < 1e-4,
            "half way between 0 and 1 radians is {}",
            state.yaw
        );
        // The newest snapshot's answers, not a blend of two.
        assert_eq!(state.health, 35);
        assert_eq!(state.action, MobAction::Windup);
        assert_eq!(state.kind, MobKind::Draugr);
    }

    /// A body that has just come into view has no earlier position to come from.
    #[test]
    fn a_mob_only_in_the_newest_snapshot_is_placed_where_it_is() {
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(with_mobs(1, vec![]), start);
        buffer.accept(
            with_mobs(2, vec![mob(900, 7.0, 0.25, 60, MobAction::Idle)]),
            start + INTERVAL,
        );

        let drawn = buffer.sample_mobs(start + INTERVAL + INTERVAL / 2, INTERVAL);
        assert_eq!(drawn.len(), 1);
        assert_eq!(drawn[0].1.pos.x, 7.0);
        assert_eq!(drawn[0].1.yaw, 0.25);
    }

    /// The newest snapshot is the complete set. An omitted id is absent immediately, and
    /// this module does not ask why.
    #[test]
    fn a_mob_the_newest_snapshot_omits_is_not_drawn() {
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(
            with_mobs(1, vec![mob(900, 0.0, 0.0, 60, MobAction::Chase)]),
            start,
        );
        buffer.accept(with_mobs(2, vec![]), start + INTERVAL);

        assert!(buffer.sample_mobs(start + INTERVAL, INTERVAL).is_empty());
    }

    /// One snapshot is not a segment.
    #[test]
    fn the_first_snapshot_of_a_session_is_drawn_exactly() {
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(
            with_mobs(1, vec![mob(900, 4.0, 0.75, 60, MobAction::Idle)]),
            start,
        );

        let drawn = buffer.sample_mobs(start + INTERVAL, INTERVAL);
        assert_eq!(drawn.len(), 1);
        assert_eq!(drawn[0].1.pos.x, 4.0);
        assert_eq!(drawn[0].1.yaw, 0.75);
    }
}
