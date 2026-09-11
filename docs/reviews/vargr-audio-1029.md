# Vargr physical audio — #1029

Twenty-two procedural recipes replace the guardian's generic Windup bark. Other
species and confirmed `BlowLanded` target-material impacts retain their routing.
No recorded assets, dependencies, mixer settings, damage rules or geometry change.

## Physical cues

| Signal | Authoritative trigger / presentation |
| --- | --- |
| Notice | Observed Idle → Chase: chest breath and iron pull; first-seen chase is silent. |
| Bite 1 / 2 | Separate Telegraph loads, Release snap/tear, Recovery breath; the announced combo tuple selects each blow. |
| Claws | Raised-paw grit, side-specific Release sweep, Recovery settling. Each combo blow has its own cue; the single claw uses the rig's right side. |
| Charge | Scrapes at preparation .18/.58, chain tension at .82, Release rattle and rig-driven footfalls, one heavy Recovery stop. |
| Leap | Telegraph strain, Release launch, landing on the final Release tick. Landing may ring into the same instance's Recovery. |
| Heavy jaws | Long gape growl, short closure, Recovery breath. Closure does not imply a hit. |
| Phase two | One strap tear on observed ordinal 1 → 2 in the same encounter, even with no moves. First-seen phase two is silent. |
| Death | One exhale on observed live → Dying or Corpse; no repeat at Dying → Corpse or first-seen corpse. Health is not read. |
| Gait | Actual completed support exchanges produce paired footfalls. No sound for spawning, correction, forced replant or attack posing. |

Charge uses one stop sound for ordinary and monolith recovery: the contract has no
impact-cause event. Durations never classify a cause. Leap landing describes an area
impact, not literal paw contact across the disc. [#1028's reach limitations](vargr-choreography-1028.md)
remain for #1037. Only the existing `BlowLanded` consumer confirms target contact;
that event does not identify a particular boss move.

## Lifetime and budget

Current timeline identities and consumed marker bits deduplicate snapshot/re-render
updates. Attack timing uses server ticks. Late entry selects the announced step,
consumes missed markers silently and retains future markers. Freshness is bounded by
`floor(tick_rate / 10)`; below 10 Hz only the marker's tick qualifies. Comparisons wrap.
Cancelled, empty, expired, replaced, dead or despawned state invalidates move sounds.
Session/snapshot loss clears history. Device-rate changes discard rings and rebake.
Missing output/camera, distance rejection and refused claims never queue a retry.
Confirmed contact tails retain their separate bounded lifetime.

The cosmetic support stamp and `Mob` accessor live entirely in `mobs/guardian.rs`.
Audio reads after the production rig sampler and encounter reconciliation; it cannot
influence movement. No shared `mobs.rs` or king edits. Placement retains 32-block
attenuation, occlusion and listener orientation. Effort/paw cues follow the boss;
footfalls and confirmed contacts stay at their occurrence positions.

Guardian voices are capped at three per boss/four globally; low-priority foot/chain
texture yields first. Recipe layers share a source. Authored source gain 0.5 leaves
headroom for correlated party gestures. The sixteen-slot mixer, eight reserved voice
slots, user SFX gain and stealing policy remain unchanged.

## Reproduction and inspection

From `<worktree>/client`:

```bash
VOXELHEIM_AUDIO_REVIEW_DIR=<review-output> cargo test --locked export_guardian_audio -- --ignored --nocapture
```

The ignored exporters open no device. All files use real synthesis, `Playback` and
`Mixer::render`; scenario tracks also run production snapshot-applied rig, encounter
and audio systems with a fixed review listener. WAVs are stereo PCM16 at 48 kHz;
adjacent CSVs record granted cue starts, ticks and origins. These are QA outputs,
not game assets. `guardian-catalogue` contains all spaced recipes.

Scenario basenames: `bite-combo`, `single-claw`, `claw-combo`, `charge`, `leap`,
`heavy-jaws`, `notice-walk-phase-death`, `cancel-late-second-replaced`,
`claws-13-blocks`, `claws-25-blocks`, `claws-muted`, `left-listener`, `right-listener`,
`jaws-open-room`, `jaws-stone-wall`, `party-four-bosses-eight-reference-voices`.
The party track uses eight synthetic steady Voice reference tones, not speech.

Fixtures follow `encounter_moves.go` at 60 Hz: bite 54/12, claw 60/18, charge 72/54,
leap 60/36, jaws 90/18 Telegraph/Release ticks; recovery 24 between blows, 108 after
bite/claw combos, 84 for single claw, 96 leap, 150 jaws. Charge travels at 11 blocks/s;
leap at 12 to its locked 6.5-block target. Production reads announced durations.

**No listening pass was performed.** The agent's audio-input path explicitly rejected
input. At closure the owner folded the manual listening pass into #1037. That issue's
[acceptance record](dungeon-acceptance-1037.md#audio) re-exports these takes and measures
them against their manifests, and it still makes no listening claim. No perceived acoustic
quality is claimed here. The corrected catalogue supersedes its initial level. Signal
metrics and manifests do not substitute for audition.

## Validation

Tests exercise actual mixer output/one-shot behavior, late second claw, skipped markers,
wrapping/1 Hz ticks, cancellation/empty/expiry/replacement, landing tail, first sight
versus notice/phase/death transitions, device changes/refusal/unavailable output,
confirmed contacts, party/Voice bounds, and actual rig contacts versus correction.
Muted SFX preserves identical production hazard geometry and announcements.
Recipes cover 8/44.1/48/192 kHz, finite samples, silent endpoints and clipping bounds.

Corrected export peaks: near claw .0656; 13 blocks .0106; 25 blocks .0020; mute 0;
jaws room .0806 versus wall .0449; four bosses plus Voice references .4268.
Observed maximum guardian sources: charge 3, leap 2, party 4. Distant audible readability
still needs listening. 29 focused checks and both exporters pass; automation passes.
Full client fmt/clippy/build/test pass (2,389 passed,16 ignored); manual listening
remains pending. Initial estimate 65–80k; fixture/report consolidation retains all
checks under the 90k reviewer cap.
