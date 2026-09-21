//! Bounded candle selection. Geometry and emission remain visible when a light is not selected.

/// Candle shadow maps have six faces each. Eight is an upper limit, not a device promise.
pub(super) const MAX_LIGHTS: usize = 8;
pub(super) const RANGE: f32 = 24.0;
const INTERVAL: f64 = 0.2;
const MAX_VISIBILITY_RAYS: usize = 32;
const RETENTION_METRES: f32 = 1.5;

#[derive(Clone, Copy, Debug)]
pub(super) struct Candidate {
    pub id: u64,
    pub distance: f32,
    pub visible: bool,
}

/// Selection is stable at equal distances and retains neighbours near a budget boundary.
#[derive(Default, Debug)]
pub(super) struct Selection {
    pub ids: Vec<u64>,
    next_update: f64,
}

impl Selection {
    /// Refine frustum visibility with bounded shape-aware sight tests only when ranking is due.
    /// The caller supplies a voxel ray from the camera to this fixture's flame; this is cosmetic.
    /// Untested and obstructed candidates retain no visibility priority, but may fill spare slots.
    pub fn refine_visibility(
        &self,
        seconds: f64,
        candidates: &mut [Candidate],
        mut unobstructed: impl FnMut(u64) -> bool,
    ) -> usize {
        if !seconds.is_finite() || seconds < self.next_update {
            return 0;
        }
        let mut order: Vec<_> = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.visible && c.distance.is_finite() && c.distance >= 0.0 && c.distance <= RANGE
            })
            .map(|(i, c)| (i, c.distance, c.id))
            .collect();
        order.sort_unstable_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)));
        for candidate in candidates.iter_mut() {
            candidate.visible = false;
        }
        let mut traced = 0;
        for (i, _, id) in order.into_iter().take(MAX_VISIBILITY_RAYS) {
            candidates[i].visible = unobstructed(id);
            traced += 1;
        }
        traced
    }

    /// Call every frame: removals, distance and capacity limits apply immediately.
    /// Returns true when the pool must publish a changed or newly ranked selection.
    /// Only ranking/admission is throttled; hard bounds never wait for the timer.
    /// `array_layers` is the remaining point-shadow capacity after reserving other shadowed lights.
    pub fn update(&mut self, seconds: f64, array_layers: u32, candidates: &[Candidate]) -> bool {
        let budget = MAX_LIGHTS.min(array_layers as usize / 6);
        let old_count = self.ids.len();
        self.ids.retain(|id| {
            candidates.iter().any(|c| {
                c.id == *id && c.distance.is_finite() && c.distance >= 0.0 && c.distance <= RANGE
            })
        });
        self.ids.truncate(budget);
        let clamped = self.ids.len() != old_count;
        if !seconds.is_finite() || seconds < self.next_update {
            return clamped;
        }
        self.next_update = seconds + INTERVAL;
        let mut ranked: Vec<_> = candidates
            .iter()
            .filter(|c| c.distance.is_finite() && c.distance >= 0.0 && c.distance <= RANGE)
            .map(|c| {
                let retained = self.ids.contains(&c.id);
                let distance =
                    (c.distance - if retained { RETENTION_METRES } else { 0.0 }).max(0.0);
                (c.id, c.visible, distance)
            })
            .collect();
        ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.2.total_cmp(&b.2)).then(a.0.cmp(&b.0)));
        self.ids.clear();
        for (id, _, _) in ranked {
            if self.ids.len() == budget {
                break;
            }
            if !self.ids.contains(&id) {
                self.ids.push(id);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: u64, distance: f32) -> Candidate {
        Candidate {
            id,
            distance,
            visible: true,
        }
    }

    #[test]
    fn sight_tests_are_bounded_throttled_and_prioritize_clear_nearby_routes() {
        let mut state = Selection::default();
        let mut candidates: Vec<_> = (1..=64).rev().map(|id| candidate(id, 8.0)).collect();
        let mut seen = Vec::new();
        assert_eq!(
            state.refine_visibility(0.0, &mut candidates, |id| {
                seen.push(id);
                id > 24
            }),
            MAX_VISIBILITY_RAYS
        );
        assert_eq!(seen, (1..=32).collect::<Vec<_>>());
        state.update(0.0, 48, &candidates);
        assert_eq!(state.ids, (25..=32).collect::<Vec<_>>());
        assert_eq!(
            state.refine_visibility(0.1, &mut candidates, |_| panic!("not due")),
            0
        );
    }

    #[test]
    fn capacity_and_range_shrink_immediately_between_ranking_updates() {
        let mut state = Selection::default();
        let nearby = [candidate(1, 2.0), candidate(2, 3.0)];
        state.update(0.0, 12, &nearby);
        assert!(state.update(0.01, 6, &nearby));
        assert_eq!(state.ids, vec![1]);
        assert!(state.update(0.02, 6, &[candidate(1, 24.01), candidate(2, 3.0)]));
        assert!(state.ids.is_empty());
        assert!(
            !state.update(0.03, 12, &nearby),
            "admission still waits for ranking"
        );
        assert!(state.update(0.2, 12, &nearby));
        assert_eq!(state.ids, vec![1, 2]);
    }

    #[test]
    fn selection_obeys_distance_device_capacity_and_stable_ties() {
        let mut state = Selection::default();
        let mut candidates: Vec<_> = (1..=20).rev().map(|id| candidate(id, 10.0)).collect();
        candidates.extend([
            candidate(0, 25.0),
            candidate(21, f32::NAN),
            candidate(22, -1.0),
        ]);
        assert!(state.update(0.0, 256, &candidates));
        assert_eq!(state.ids, (1..=8).collect::<Vec<_>>());
        assert!(state.update(0.2, 12, &candidates));
        assert_eq!(state.ids, vec![1, 2]);
        assert!(state.update(0.4, 5, &candidates));
        assert!(state.ids.is_empty());
    }

    #[test]
    fn selection_is_throttled_but_removed_fixtures_leave_immediately() {
        let mut state = Selection::default();
        state.update(0.0, 6, &[candidate(1, 4.0)]);
        assert!(!state.update(0.10, 6, &[candidate(1, 4.0), candidate(2, 1.0)]));
        assert_eq!(state.ids, vec![1]);
        assert!(state.update(0.19, 6, &[]));
        assert!(state.ids.is_empty());
    }

    #[test]
    fn nearby_retained_light_does_not_churn_at_an_equidistant_boundary() {
        let mut state = Selection::default();
        state.update(0.0, 6, &[candidate(2, 10.0)]);
        state.update(0.2, 6, &[candidate(1, 9.8), candidate(2, 10.0)]);
        assert_eq!(state.ids, vec![2]);
        state.update(0.4, 6, &[candidate(1, 7.0), candidate(2, 10.0)]);
        assert_eq!(state.ids, vec![1]);
    }

    #[test]
    fn visible_routes_take_priority_and_duplicate_candidates_never_duplicate_lights() {
        let mut state = Selection::default();
        state.update(
            0.0,
            12,
            &[
                Candidate {
                    id: 1,
                    distance: 1.0,
                    visible: false,
                },
                candidate(2, 2.0),
                candidate(2, 2.0),
                candidate(3, 3.0),
            ],
        );
        assert_eq!(state.ids, vec![2, 3]);
    }
}
