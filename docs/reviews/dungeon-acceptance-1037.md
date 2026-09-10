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

**No listening claim is made.** The agent has no audio input. The measurements above establish that
each cue is present in the mix on its manifested tick, attenuates with distance, and is silent when
muted. They do not establish how any cue sounds to a player, whether cues are distinguishable by
ear, or whether they read at distance. The manual inspection #1029 and #1035 deferred here is
delivered as these recordings and measurements, not as an audition.

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
- **Budget:** there is no recorded server cost budget. The reference is the tick interval, 50 ms at
  the default 20 Hz.

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

<!-- pending: the rendering cost harness in the shipped chamber -->

## Recorded budgets

The design's [initial authoring budgets](../first-dungeon/README.md#initial-authoring-budgets) are
the only recorded reference budgets. It states that a final GPU performance budget remains a
measured follow-up.

<!-- pending: measured segments, triangles, materials and concurrent effect groups -->

## Acceptance criteria

<!-- pending -->
