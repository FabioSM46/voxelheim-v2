# Draugr king voice and blade audio, part 1 of 2 (#1035)

Fifteen procedural recipes replace the king's generic notice and Windup barks. The
guardian's observer from [#1029](vargr-audio-1029.md) is shared rather than copied: a
boss `Voice` names a species' catalogue, markers, anchors and stage cue, and
observation, freshness, ownership and source caps stay in one place. Part 2 adds the
Sepulchre Spear and the Burial, Edict and Requiem casts and channels through the same
table. No recorded assets, dependencies, mixer settings, schema, server or gameplay rule
change.

## Cues

| Signal | Authoritative trigger / presentation |
| --- | --- |
| Notice | Observed Idle → Chase: hollow groan and iron. First-seen chase is silent. |
| King's Sentence | Telegraph: blade raised at 0, a low clang at .80 inside the held pause. Release: heavy cut at 0 and the blade biting the floor on the final tick. Recovery: blade pulled free at .55, where the choreography starts to lift it. |
| Three Tolls | Each telegraph rings its own bell (233, 175, 117 Hz) chosen by the announced combo step. Releases: a low left cut, a sharper right cut, a forward thrust, placed on the side of the announced region. Only the final step's recovery settles. |
| Final stage | One mask clatter on the first observed stage-3 timeline after an earlier stage of the same encounter. First sight of stage 3 or a new encounter id is silent, as the regalia are. |
| Death | One collapse on observed live → Dying or Corpse. Health is not read. |

The king's phases are short at 20 Hz (a 200 ms release is four ticks), so each king
cue is owned by its move instance rather than its phase: it rings on through later
phases of that instance and stops on cancellation, replacement, expiry or death. A
gap in timelines no longer resets the observed stage; only an announcement does.

## Lifetime and budget

Unchanged from #1029 and shared: markers consume once per phase identity, stay fresh for
`floor(tick_rate / 10)` ticks (two at 20 Hz), wrap, and never catch up after a late
snapshot. Placement keeps 32-block attenuation, occlusion and listener yaw. Boss cues are
capped at three per boss and four across every boss; admission may evict only a lower
priority boss cue. Reads, clangs, bells, the floor bite, the mask and death outrank
whooshes and settling. The sixteen-slot mixer, eight reserved Voice slots and SFX gain
are untouched.

## Reproduction and inspection

From `<worktree>/client`:

```bash
VOXELHEIM_AUDIO_REVIEW_DIR=<review-output> cargo test --locked export_king_audio -- --ignored --nocapture
```

The ignored exporters open no device and use real synthesis, `Playback` and
`Mixer::render`; the sequences also run the production snapshot-applied rig, encounter
reconciliation and audio systems. Each 20 Hz tick is mixed as three 60 Hz frames of one
snapshot. WAVs are stereo PCM16 at 48 kHz with an adjacent CSV of granted cue starts,
ticks and origins. They are QA outputs, not game assets.

Basenames: `king-catalogue`, `sentence`, `sentence-stone-wall`, `three-tolls`,
`tolls-13-blocks`, `tolls-25-blocks`, `tolls-muted`, `notice-mask-death`,
`cancel-late-replaced`. Fixtures follow `encounter_moves.go` and `encounter_combo.go` at
20 Hz: Sentence 24/5/36 and each toll 18/4 ticks, with 8 between blows and 44 after the
third. Production reads the announced durations.

Recorded manifests: the Sentence starts at ticks 101, 119, 125, 129 and 149; the tolls
place the first cut at x −0.82 and the second at +0.98; the mask falls at tick 140 and the
corpse at 200. In `cancel-late-replaced` the Sentence cancelled at tick 110 never clangs,
the second toll first seen eight ticks late plays no bell but plays its cut at 140, and a
new Sentence replaces it at 146.

Export peaks: catalogue .1651; Sentence .1007 versus behind stone .0826; tolls .1098,
13 blocks .0177, 25 blocks .0033, muted 0; notice/mask/death .1470. Maximum boss sources:
Sentence 3, tolls 2.

**Manual listening is pending.** No listening or perceived acoustic quality is claimed.
Actual listener feedback or an explicit user decision remains required, and signal metrics
and manifests do not substitute for audition. Every cue duplicates information already on
screen (pose, labels, countdowns and hazard geometry); none is necessary to survive.

## Validation

Tests cover every blade marker on its announced tick with no generic bark and no replay on
a held snapshot, two-tick freshness at 20 Hz, tick wrap, tails ringing into recovery and
ending on replacement, cancellation, first sight versus notice, stage and death, left/right
panning, distance and range rejection, shared caps with eight Voice slots free, muting
without replay or presentation change, and recipe bounds at 8/44.1/48/192 kHz. The guardian
suite is unchanged apart from the shared cap's name.
