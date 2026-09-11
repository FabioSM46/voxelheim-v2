# First dungeon audio and performance acceptance (#1037, final part)

[Part 1](dungeon-combat-1037.md) measured the fights, [part 2](dungeon-corrections-1037.md)
corrected latency room and strike reach, [part 3a](boss-strikes-1037.md) presented strike reach,
and [part 3b](bosses-in-motion-1037.md) captured both models in motion in the shipped chamber.
This part records the audio exports, the muted-audio check, and the server and rendering cost
against the recorded budgets. It changes no schema, gameplay rule, mixer setting, recipe, model or
timing.

## Reference machine

- **CPU:** AMD Ryzen 7 3700X, 16 logical CPUs, x86_64 Linux.
- **GPU:** AMD Radeon RX 5700 XT (RADV NAVI10), Vulkan, Mesa 25.2.8.
- **Toolchains:** Go 1.26.6 and the repository's pinned Rust toolchain.

The host is a shared workstation, not an isolated benchmark rig. Each measurement below says what
else was running and how many runs it covers.

## Audio

### Exports

From `<worktree>/client`, the existing exporters from #1029 and #1035 were re-run on this branch:

```sh
VOXELHEIM_AUDIO_REVIEW_DIR=<review-output> cargo test --locked export_guardian_audio -- --ignored --nocapture
VOXELHEIM_AUDIO_REVIEW_DIR=<review-output> cargo test --locked export_king_audio -- --ignored --nocapture
```

They wrote 33 takes: stereo PCM16 WAVs at 48 kHz, each with a timestamp manifest. They render
through real synthesis, `Playback`, `Mixer::render` and the production snapshot, encounter and audio
systems, and open no device. The WAVs are QA outputs and are not committed; the take list and
fixtures are in [the Vargr record](vargr-audio-1029.md) and [the Draugr record](draugr-audio-1035.md).
The guardian takes follow the server catalogue's durations at 60 Hz, which still match
`encounter_moves.go` after part 2; the shipped server runs at 20 Hz, which the king takes use.

### Measured against the manifests

[`onsets.py`](dungeon-acceptance-1037/onsets.py) reads each WAV beside its manifest, using only the
Python standard library:

```sh
python3 docs/reviews/dungeon-acceptance-1037/onsets.py <review-output>
```

The results are in [`audio-onsets.csv`](dungeon-acceptance-1037/audio-onsets.csv). In 10 ms frames it
records the peak, the level in the 40 ms after each manifested cue start, and every onset, meaning
a frame at least 9 dB above the previous 50 ms and above −50 dBFS.

- **Every onset is manifested.** No take has an onset that does not fall within 80 ms after a
  manifested cue start.
- **Every manifested cue is present in the near takes.** In the catalogues and every take at
  3 blocks, a cue's first 40 ms reach −50 dBFS except for eleven starts, all slow-attack recipes.
  Each of the eleven rises above −50 dBFS within 20–160 ms of its manifested start. One of them is
  in a 13-block take and is included for completeness:

  | Cue | Take | Rises above −50 dBFS after |
  | --- | --- | ---: |
  | Guardian jaws load | behind the stone wall | 20 ms |
  | King's Sentence, blade raised | open room / behind stone / catalogue | 40 / 60 / 30 ms |
  | King's Sentence, blade raised | both Sentences in `cancel-late-replaced` | 40 / 40 ms |
  | King's Sentence, blade pulled free | open room / behind stone | 30 / 100 ms |
  | Sepulchre Spear, crystal gathering | spear / catalogue | 160 / 150 ms |
  | Requiem, first note | 13 blocks | 50 ms |

- **Distant takes are not measured for presence.** At 13 and 25 blocks, many cue starts stay under
  the −50 dBFS floor for their first 40 ms: 4 of 6 claws and 3 of 7 tolls at 13 blocks, every cue at
  25 blocks. That measures attenuation. Whether those cues are audible at that distance was not
  measured, and no claim is made either way.

- **Distance attenuates.** Peaks fall from .0656 to .0106 to .0020 for the claws at 3, 13 and
  25 blocks, and from .1098 to .0177 to .0033 for the tolls.
- **The mixer does not clip.** The loudest take is four bosses plus eight reference Voice tones,
  at a peak of .4268.

### Muted audio

Both muted takes, `claws-muted` and `tolls-muted`, have a peak of exactly 0 across their whole
length. Their manifests list the same number of granted cues as the audible takes of the same
sequence: 6 for `claw-combo` and 7 for `three-tolls`. With the SFX bus at zero, the audio systems
still consumed each marker.

What a player can see without sound is pinned by existing tests:

- `muting_leaves_the_same_announced_hazard_meshes_and_authoritative_state` (Vargr): muting leaves
  the same hazard meshes and state.
- `muting_keeps_the_presentation_and_unmuting_replays_nothing` (Draugr): muting keeps the
  presentation, and unmuting replays nothing.
- `unavailable_output_and_muting_consume_markers_without_later_replay`: muted markers never play
  later.

That the announcement precedes damage does not depend on the client at all. Part 1's
`TestFirstDungeonDamageNeverPrecedesItsPerceivedAnnouncement` checks it from the frames a player's
session is delivered, and parts 1 and 2 swept delayed and lost snapshots.

### Listening

**These are recordings and signal measurements against the manifests, not a listening claim.** The
agent has no audio input.

**What the measurements establish:** each cue is present in the mix on its manifested tick, it
attenuates with distance, and it is silent when muted.

**What they do not establish:** how any cue sounds to a player, whether cues can be told apart by
ear, or whether they read at distance.

The owner decided that #1037 is completed with no separate human inspection. The listening pass deferred
from #1029 and #1035 is therefore met by this recorded evidence, stated as not being a listening
claim: the [manifest analysis](dungeon-acceptance-1037/audio-onsets.csv) and the exporters that
regenerate the WAVs and manifests.

## Server cost

From `<worktree>/server`:

```sh
VOXELHEIM_SERVER_COST=1 VOXELHEIM_SERVER_COST_DIR=<output> \
    go test ./internal/game/ -run '^TestFirstDungeonServerCost$' -count=1 -v
```

`TestFirstDungeonServerCost` times the whole `InstanceManager.Step` for a session holding the
dungeon and its party: movement, collision, the encounter scheduler, hazards, damage and snapshots.
It uses part 1's harness, whose scripted players decode their own frames outside the timed call.

- **Scenarios:** each boss stage is fought for 120 simulated seconds (2,400 ticks at 20 Hz) by
  evading parties of one and four, with ranged members so the distance moves are chosen. Every
  stage's repertoire was performed. The baseline is the same session with both bosses dead.
- **Warm-up:** one discarded cleared-dungeon run precedes the recorded ones.
- **Runs:** three consecutive runs with nothing else started by this work; the host's one-minute
  load average was 1.7–1.8.
- **Reference:** no server cost budget was set. The owner's decision is to report the cost against
  the tick interval, 50 ms at the default 20 Hz, and to record these numbers as the baseline for
  later comparison, not as a pass or fail against a threshold.

Across the three runs ([run 1](dungeon-acceptance-1037/server-cost-run1.csv),
[2](dungeon-acceptance-1037/server-cost-run2.csv), [3](dungeon-acceptance-1037/server-cost-run3.csv)):

| Scenario | Mean | p99 | Max |
| --- | ---: | ---: | ---: |
| Cleared, party of one | 5.7–6.6 µs | 10.8–14.1 µs | 107–140 µs |
| Cleared, party of four | 29.7–42.3 µs | 62.7–63.5 µs | 261–292 µs |
| Vargr fights, party of one | 11.3–14.2 µs | 50.7–52.1 µs | 158–355 µs |
| Vargr fights, party of four | 30.7–40.4 µs | 69.7–116.1 µs | 164–481 µs |
| Draugr fights, party of one | 9.8–16.5 µs | 22.3–53.2 µs | 258–683 µs |
| Draugr fights, party of four | 28.1–39.5 µs | 66.2–99.5 µs | 194–818 µs |

The highest p99 is 116 µs, 0.23% of the tick interval; the highest single step is 818 µs, 1.6%.
The party size, not the encounter, dominates: a fight adds a few microseconds per tick to a solo
session, and at four players the fights and the cleared baseline overlap within run-to-run
variation. This is one dungeon session on one process; it makes no claim about many concurrent
instances.

## Rendering cost

From `<worktree>/client`:

```sh
WGPU_BACKEND=vulkan cargo test --locked \
    player::mobs::arena_capture::measure_rendering_cost_in_the_shipped_chamber -- --ignored --exact
```

`measure_rendering_cost_in_the_shipped_chamber` uses [part 3b](bosses-in-motion-1037.md)'s scene.
The shipped chamber is drawn by the production chunk mesher, and the bosses by the production
snapshot, animation, regalia, encounter and effect systems.

- **Frame:** one `App::update` at 60 Hz, then a wait until the GPU has finished the work that frame
  submitted. The time therefore includes GPU work, not only its submission.
- **What it is not:** it is rendered offscreen at 1280 × 720 with the default field of view, so there
  is no presentation, vsync or compositor. The lighting is part 3b's review lighting, unshadowed.
- **Scenes:** each scene settles for 120 frames, then 900 frames are timed. The camera is at a
  standing player's eye height, eight blocks in front of the boss.
  - The empty courtyard and the empty hall.
  - The Vargr idle, looping its paired claws in stage two, and looping its leap.
  - The Draugr idle, looping the Spear in stage one, Burial in stage two, and Requiem in the final
    stage.
- **Pacing:** snapshots arrive at 20 Hz and each phase is announced on the tick it begins, at the
  catalogue's durations.
- **Runs:** three consecutive runs with no other build or capture running; the host's one-minute
  load average was 1.8–2.3. A PNG of each boss scene was inspected after run 3 to confirm the boss
  and its effects were drawn.
- **Reference:** no frame-time budget was set. These numbers are the baseline for later comparison,
  not a pass or fail.

Across the three runs ([run 1](dungeon-acceptance-1037/render-cost-run1.csv),
[2](dungeon-acceptance-1037/render-cost-run2.csv), [3](dungeon-acceptance-1037/render-cost-run3.csv)):

| Scene | Mean | p50 | p99 | Max |
| --- | ---: | ---: | ---: | ---: |
| Empty courtyard | 4.66–4.80 ms | 4.61–4.75 ms | 5.52–6.08 ms | 5.73–10.29 ms |
| Empty hall | 4.51–4.67 ms | 4.51–4.58 ms | 5.35–5.83 ms | 5.63–6.67 ms |
| Vargr idle | 4.81–4.87 ms | 4.72–4.79 ms | 5.61–5.92 ms | 6.20–7.42 ms |
| Vargr paired claws | 4.85–4.98 ms | 4.91 ms | 5.86–6.00 ms | 6.52–13.14 ms |
| Vargr leap | 5.01–5.06 ms | 4.92–4.98 ms | 5.85–5.93 ms | 6.13–10.23 ms |
| Draugr idle | 4.81–4.85 ms | 4.72–4.80 ms | 5.70–6.14 ms | 5.91–9.11 ms |
| Draugr Spear | 4.74–5.20 ms | 4.81–5.10 ms | 5.61–6.35 ms | 5.99–12.95 ms |
| Draugr Burial | 4.98–5.03 ms | 4.95–4.98 ms | 5.75–5.87 ms | 6.02–6.34 ms |
| Draugr Requiem, final stage | 4.96–5.07 ms | 4.89–4.97 ms | 5.88–6.08 ms | 6.29–8.98 ms |

- **Mean cost of a boss:** against the empty chamber in the same run, an idle boss adds 0.07–0.15 ms
  mean for the Vargr and 0.18–0.30 ms for the Draugr. A boss looping a move adds up to 0.35 ms
  (Vargr) and 0.53 ms (Draugr). The empty chamber itself varies by up to 0.16 ms mean between runs.
- **p99:** stays within 5.3–6.4 ms in every scene.
- **Max:** the single slowest frames, up to 13.14 ms once in a Vargr paired-claws run, did not recur
  across runs and also occur in the empty courtyard (10.29 ms once); this is a shared workstation.
- **Scope:** one boss in view with its effects. No party of players, other mobs, shadows or the
  dungeon's own lighting are included, and nothing here predicts frame rate on other hardware.

## Recorded budgets

The design's [initial authoring budgets](../first-dungeon/README.md#initial-authoring-budgets) are
the only recorded reference budgets. The owner treats them as real limits, so the actual counts
below are checked against them:
- moving mesh segments;
- triangles;
- material handles;
- concurrent cosmetic effect groups.

The design sets no frame-time budget and makes no FPS promise without hardware. The frame times
above are measured on the named reference machine and recorded as the baseline for later
comparison, not as a pass or fail.

Counts are taken every timed frame of the scenes above, and each row is the largest value seen. The
harness fails if any count exceeds its cap.
- **Segments:** the boss's visible rig meshes.
- **Triangles:** the triangles of those segments.
- **Materials:** the distinct material handles on those segments and on visible regalia.
- **Effect groups:** each move instance a spell or strike layer draws for the boss, plus the king's
  core glow and hand crystal while visible. Telegraph outlines and the encounter reading are not
  counted: the [#1028 review](vargr-choreography-1028.md) records that cues and UI keep their own
  budgets.

| Boss | Segments | Triangles | Materials | Concurrent effect groups |
| --- | --- | --- | --- | --- |
| Vargr | 19 of 19 | 648 of 12,000 | 1 of 2 | 1 of 2 (paired claws) |
| Draugr | 17 of 17 | 1,464 of 12,000 | 2 of 2 (Spear, Burial, final-stage Requiem) | 2 of 4 (the same scenes) |

- **Segments:** both bosses use every segment their cap allows.
- **Triangles:** at most 12.2% of the triangle cap.
- **The Draugr:** its second material comes from its regalia, and its effect groups from the regalia
  and its spell shapes.
- **The Vargr:** idle and the leap draw no effect group.

## Remaining limits

- **The Draugr's blade enters adjacent terrain.** [Part 3b](bosses-in-motion-1037.md) measured 24–96
  vertices inside the monolith in the Sentence and Toll frames at 1.1 blocks from it. This part
  changes no blade pose. The limit is tracked by #1103, a presentation-only blade pose that avoids
  terrain.
- **Reduced visual effects** stays blocked on #1093; no stand-in setting was built.
- **Fight length** is handed to #1099. Part 1 measured every fight far shorter than the design's
  targets, and no health, damage or scaling value changes here.
- **No listening claim** is made about any cue, as stated under [Listening](#listening).

## Acceptance criteria

Each criterion of #1037, with the test or recorded evidence that proves it.

1. **Party sizes, entry equipment, kill times, escape paths, punish windows, tuning changes.**
   - **Measurement:** [part 1](dungeon-combat-1037.md), from `TestFirstDungeonPlaytest`, covering
     parties of one to four, three entry kits and every stage, with delay and loss.
   - **Escapability:** `TestFirstDungeonReadersEscapeEveryAnnouncedRegion` and
     `TestFirstDungeonEveryStageRepertoireIsEscapable`.
   - **Tuning changes:** recorded in [part 2](dungeon-corrections-1037.md), with
     `TestPlantedBlowsReachNoFartherThanTheirEngagement` and
     `TestAPlantedMoveIsChosenOnlyWhereItsRegionReachesTheTarget`.
   - **Fight length:** handed to #1099 at the owner's decision.
2. **Delayed and lost snapshots, reduced visual effects, muted audio; telegraphs before damage; no
   unavoidable overlap.**
   - **Delay and loss:** part 1's network sweep and `TestFirstDungeonRitualPulsesSurviveLatency`.
   - **Telegraphs before damage:** `TestFirstDungeonDamageNeverPrecedesItsPerceivedAnnouncement`.
   - **No unavoidable overlap:** `TestASelectionThatCoversEveryEscapeIsRefused` and
     `TestFirstDungeonEveryStageRepertoireIsEscapable`.
   - **Muted audio:** the tests and muted takes under [Muted audio](#muted-audio).
   - **Reduced visual effects:** blocked on #1093, with no stand-in setting.
3. **Final models in motion, feet and weapon clipping, rendering and server cost against the recorded
   budgets; demonstrated issues corrected; remaining limits documented.**
   - **Strike presentation:** [part 3a](boss-strikes-1037.md), with
     `every_strike_stays_inside_its_announced_volume_and_reaches_its_boundary_on_the_last_release_tick`.
   - **Models in motion and clipping:** [part 3b](bosses-in-motion-1037.md), with
     `capture_bosses_in_the_shipped_chamber` and the corrected corpse fall pinned by
     `a_corpse_folds_away_from_terrain_it_would_otherwise_lie_in`.
   - **Cost:** [rendering cost](#rendering-cost) and the [authoring caps](#recorded-budgets), from
     `measure_rendering_cost_in_the_shipped_chamber`; [server cost](#server-cost), from
     `TestFirstDungeonServerCost`.
   - **Listening pass deferred from #1029 and #1035:** met by recordings plus signal measurements
     against the manifests, stated as not a listening claim, under [Listening](#listening).
   - **Limits:** [Remaining limits](#remaining-limits).
4. **Workspace checks.** The client and server gates and the automation suite pass on this branch.
   No schema changes.
