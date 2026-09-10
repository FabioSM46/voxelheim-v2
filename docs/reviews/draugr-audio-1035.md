# Draugr king voice, blade, spell and channel audio (#1035)

Thirty procedural recipes replace the king's generic notice and Windup barks. The
guardian's observer from [#1029](vargr-audio-1029.md) is shared rather than copied: a
boss `Voice` names a species' catalogue, markers, anchors, stage cue and interrupt cue,
and observation, freshness, ownership and source caps stay in one place. Part 1 carries
the voice and blade; part 2 the Sepulchre Spear and the Burial, Edict and Requiem casts
and channels. No recorded assets, dependencies, mixer settings, schema, server or
gameplay rule change.

## Cues

| Signal | Authoritative trigger / presentation |
| --- | --- |
| Notice | Observed Idle → Chase: hollow groan and iron. First-seen chase is silent. |
| King's Sentence | Telegraph: blade raised at 0, a low clang at .80 inside the held pause. Release: heavy cut at 0 and the blade biting the floor on the final tick. Recovery: blade pulled free at .55, where the choreography starts to lift it. |
| Three Tolls | Each telegraph rings its own bell (233, 175, 117 Hz) chosen by the announced combo step. Releases: a low left cut, a sharper right cut, a forward thrust, placed on the side of the announced region. Only the final step's recovery settles. |
| Sepulchre Spear | Telegraph: a crystal gathering, staggered long attacks rising in pitch and loudness. Release: a glassy loose at the raised hand. Recovery: settle. |
| Burial | Telegraph: the sword planted. Each pulse: cracks running on its first tick, an earth burst on its last (the server's contact tick). |
| Edict of the Graves | Telegraph: a command call. Each pulse: a rune chime whose pitch is the announced pulse index (392, 523, 698 Hz), a slab burst on its contact tick. |
| Requiem of the Buried | Telegraph: the sword planted. Each pulse: one of three descending intoned notes (147, 131, 98 Hz, no words) chosen by the pulse index, a deep toll on its contact tick. |
| Interrupt | One choked note and shattering ice, only when a channel this client watched ends `Interrupted` while the snapshot is inside that pulse's window. Cancellation, completion, a first-seen ending and a stale ending are silent. |
| Final stage | One mask clatter on the first observed stage-3 timeline after an earlier stage of the same encounter. First sight of stage 3 or a new encounter id is silent, as the regalia are. |
| Death | One collapse on observed live → Dying or Corpse. Health is not read. |

The king's phases are short at 20 Hz (a 200 ms release is four ticks, a pulse 14–18),
so each king cue is owned by its move instance rather than its phase: a contact rings on
into the next pulse or the recovery and stops on cancellation, interrupt, replacement,
expiry or death. A gap in timelines no longer resets the observed stage; only an
announcement does. Ritual cues sit on the king's planted blade or voice; the floor
geometry, not the sound, says where to stand.

## Lifetime and budget

Unchanged from #1029 and shared: markers consume once per phase identity (each pulse has
its own), stay fresh for `floor(tick_rate / 10)` ticks (two at 20 Hz), wrap, and never
catch up after a late snapshot. Placement keeps 32-block attenuation, occlusion and
listener yaw. Boss cues are capped at three per boss and four across every boss; admission
may evict only a lower-priority boss cue. Reads, clangs, bells, runes, notes, contacts, the
interrupt, the mask and death outrank whooshes, cracks and settling. The sixteen-slot
mixer, eight reserved Voice slots and SFX gain are untouched.

## Reproduction and inspection

From `<worktree>/client`:

```bash
VOXELHEIM_AUDIO_REVIEW_DIR=<review-output> cargo test --locked export_king_audio -- --ignored --nocapture
```

The ignored exporters open no device and use real synthesis, `Playback` and
`Mixer::render`; the sequences also run the production snapshot-applied rig, encounter
reconciliation, spell presentation and audio systems. Each 20 Hz tick is mixed as three
60 Hz frames of one snapshot. WAVs are stereo PCM16 at 48 kHz with an adjacent CSV of
granted cue starts, ticks and origins. They are QA outputs, not game assets.

Basenames: `king-catalogue`, `sentence`, `sentence-stone-wall`, `three-tolls`,
`tolls-13-blocks`, `tolls-25-blocks`, `tolls-muted`, `spear`, `burial`, `edict`,
`requiem`, `requiem-13-blocks`, `requiem-interrupted`, `edict-cancelled`,
`notice-mask-death`, `cancel-late-replaced`. Fixtures follow `encounter_moves.go` and
`encounter_combo.go` at 20 Hz: Sentence 24/5/36; each toll 18/4, with 8 between blows and
44 after the third; Spear 28/16/32; Burial 30, four 14-tick pulses, 40; Edict 30, three 16s,
36; Requiem 30, three 18s, 40. Production reads the announced durations.

Recorded manifests:

- The Sentence starts at ticks 101, 119, 125, 129 and 149; the tolls place the first cut at
  x −0.82 and the second at +0.98; the mask falls at 140 and the corpse at 200.
- `cancel-late-replaced`: the Sentence cancelled at 110 never clangs, the second toll first
  seen eight ticks late plays no bell but its cut at 140, and a new Sentence replaces it at 146.
- Burial pulses start at 131, 145, 159 and 173 and burst at 144, 158, 172 and 186; Edict
  runes rise pulse by pulse with bursts at 146, 162 and 178; Requiem notes descend with tolls
  at 148, 166 and 184.
- `requiem-interrupted`: the second note at 149, one break at 158, then nothing. `edict-cancelled`:
  nothing after the second rune at 147, neither its burst nor a recovery.

Export peaks: catalogue .1651; Sentence .1007 versus behind stone .0826; tolls .1098,
13 blocks .0177, 25 blocks .0033, muted 0; Spear .0653; Burial .0946; Edict .1112; Requiem
.0990, 13 blocks .0183, interrupted .0967; notice/mask/death .1470. Maximum boss sources:
Sentence 3, every other take 2 or fewer.

**No listening pass was performed.** At closure the owner folded the manual listening pass
into #1037. That issue's [acceptance record](dungeon-acceptance-1037.md#audio) re-exports these
takes and measures them against their manifests, and it still makes no listening claim. No
perceived acoustic quality is claimed here, and signal metrics and manifests do not substitute
for audition. Every cue duplicates information already on
screen (pose, cast label, countdown, hazard geometry and spell shapes); none is necessary
to survive.

## Validation

Tests cover every blade, cast and pulse marker on its announced tick with no generic bark
and no replay on a held snapshot; two-tick freshness at 20 Hz and tick wrap; tails ringing
into the next pulse or recovery and ending on replacement; a late pulse keeping only its
contact; the interrupt voiced once and never for cancellation, completion, an unwatched or
stale ending; first sight versus notice, stage and death; left/right panning, distance and
range rejection; shared caps with eight Voice slots free; muting without replay or
presentation change; and recipe bounds and distinctness at 8/44.1/48/192 kHz. The guardian
suite is unchanged apart from the shared cap's name.
