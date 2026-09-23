//! A per-species voice limit: how many sources one creature species may have playing at once
//! across all of its members, and how many of those may be its footsteps.
//!
//! A crowd above the cap is heard as its nearest few (admission is offered nearest first);
//! the rest are not mixed at all, which is what keeps a wave of creatures from summing into a
//! roar. The spider and the scorpion each hold their own cap through this one rule.
use super::Active;

/// How many of one species' sources may play at once, and how many of them may be legs.
#[derive(Clone, Copy, Debug)]
pub(super) struct Cap {
    pub(super) sources: usize,
    pub(super) legs: usize,
}

/// Whether a cue of `priority` (a footstep when `legs`) from creature `id` may start, and which
/// playing source of the same species it replaces if one must go. `Err` refuses it.
///
/// `of` reads a playing source as this species' `(priority, legs)`, or `None` for a source
/// that is not this species'. A creature plays one set of footsteps at a time; a full species
/// gives up its lowest-priority source only to something that outranks it, and a footstep
/// only ever replaces another footstep.
pub(super) fn admit(
    playing: &[Active],
    cap: Cap,
    (priority, legs): (u8, bool),
    id: u64,
    of: impl Fn(&Active) -> Option<(u8, bool)>,
) -> Result<Option<usize>, ()> {
    let is_legs = |active: &Active| of(active).is_some_and(|(_, legs)| legs);
    if legs
        && playing
            .iter()
            .any(|active| active.id == id && is_legs(active))
    {
        return Err(());
    }
    let total = playing.iter().filter(|active| of(active).is_some()).count();
    let walking = playing.iter().filter(|active| is_legs(active)).count();
    if total < cap.sources && !(legs && walking >= cap.legs) {
        return Ok(None);
    }
    playing
        .iter()
        .enumerate()
        .filter(|(_, active)| {
            of(active).is_some_and(|(other, other_legs)| other < priority && (!legs || other_legs))
        })
        .min_by_key(|(_, active)| active.priority)
        .map(|(index, _)| Some(index))
        .ok_or(())
}
