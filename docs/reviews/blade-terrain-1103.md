# Draugr blade pose beside terrain (#1103)

[#1037 part 3b](bosses-in-motion-1037.md) measured the Draugr king's blade inside a monolith in the
Sentence and Toll frames when he strikes 1.1 blocks from it: 24–96 vertices a frame. The server
legitimately lets him stand there and announces the blow along its locked aim, so the blade could
not simply be retracted. This change gives a planted blow a terrain-clear variant along the same
aim. It changes no schema, server code, move selection, attack volume, timing, wire message,
strike layer or model geometry, and nothing about the Vargr.

## The decision

It is the corpse fold's shape (#1102): chosen once, from a terrain check of every model vertex,
without moving the server-sent position.

- **What varies.** Only the blade's direction at the wrist, about the achieved grip. It either
  swings about the vertical (`yaw`) or lifts or lowers its tip (`raise`). Both are weighted by how
  horizontal the authored blade is, so a blade held upright overhead or planted straight down is
  unchanged.
  - The grip trajectory, both hands, the timing, the locked aim's torso twist and every other joint
    target stay authored.
- **The candidates** (`king::choreography::WRISTS`), in preference order:
  - the authored stroke;
  - raise ±0.5 rad, then yaw ±0.5;
  - raise ±0.9, then yaw ±0.9;
  - raise ±1.25, then yaw ±1.25.
- **The choice** (`king::motion::Motion::choose_blade`) is made when a Sentence or Toll is first
  presented, over all three phases at 21 samples each: preparation, release and recovery.
  - It takes the first candidate that puts no vertex inside solid terrain and keeps both hands on
    the weapon.
  - Failing that, it takes the candidate with the fewest vertices in terrain, and never one that
    lets go.
  - The same variant is kept for the whole blow, so no phase boundary can switch strokes.
  - A new move instance, a new combination step, or a root that moves 0.25 blocks or turns 0.1
    rad chooses again.
- **The measure** is the chamber capture's: a vertex counts when it lies at least 0.02 blocks
  inside a solid voxel and above the floor the king stands on. The authored planted blade in the
  floor is therefore unchanged.
- **The cost.** The voxels within reach, 9 × 9 × 6 above the floor, are read once per choice.
  - A king with nothing solid above his floor keeps the authored stroke without sampling a pose.
  - Otherwise the non-wielding segments are counted once, and only the blade and arms are
    re-posed per candidate.
- **The wiring.** `pose_encounters` now reads the chunk store through the same `solid_voxel` helper
  the corpse fall uses.

### What the decision is not

- **Not a different reach.** The announced reach is drawn by part 3a's strike strokes and boundary
  cues. Those run to the announced far boundary and do not read this choice. The blade model never
  reached that boundary: it is at most about 2.6 blocks from the root, against 3.8 for the Tolls
  and 5.0 for the Sentence.
- **Not a retraction.** No candidate pulls the grip back or shortens the blade; they point the same
  blade elsewhere past the same hands.
- **Not an inverse-kinematics system.** It chooses from a fixed table with the existing arm solver;
  a candidate asking an arm past its rigid reach is rejected.

## Unit measurements

Each scene puts a face 1.1 blocks from the root, the distance #1037 measured:
- **monolith:** the chamber's 1 × 4 column ahead;
- **wall:** a wall ahead;
- **corners:** the monolith ahead with a wall 1.1 blocks to the right, and to the left.

Vertices in terrain are summed over the 63 samples; `x` marks a candidate that lets go of the
weapon. Columns follow the candidate order above.

| Scene, blow | authored | r+.5 | r−.5 | y+.5 | y−.5 | r+.9 | r−.9 | y+.9 | y−.9 | r+1.25 | r−1.25 | y+1.25 | y−1.25 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| monolith, Sentence | 750 | 366 | 882 | 513 | 243 | 306 | 846 | **0** | 0 | 258 | 798 | 0 | 0 |
| monolith, first toll | 2400 | 2100 | 2349 | 1314 | 3060 | 144 | 144 | 42 | 2232 | **0** | 0 | 0 | 1386 |
| monolith, second toll | 2238 | 1518 | 1848 | 2484 | 852 | **0** | 0 | 2412 | 0 | 0 | 0 | 1776 | 0 |
| monolith, third toll | 4596 | 3762 | 4044 | 3474 | 708 | 1032x | 1194 | **0** | 0 | 0x | 0 | 0x | 0 |
| wall, Sentence | 750 | 366 | 882 | 513 | 411 | 306 | 846 | 33 | 33 | 258 | 798 | **0** | 0 |
| wall, first toll | 3192 | 2433 | 2709 | 1950 | 3756 | 144 | 144 | 900 | 2754 | **0** | 0 | 78 | 1914 |
| wall, second toll | 3024 | 1869 | 2217 | 3366 | 1788 | **0** | 0 | 2556 | 720 | 0 | 0 | 1854 | 24 |
| wall, third toll | 4596 | 3762 | 4044 | 3474 | 3180 | 1032x | 1194 | 1200 | 1200 | 0x | **0** | 0x | 0 |
| corner right, Sentence | 375 | 183 | 441 | 519 | **0** | 153 | 423 | 33 | 0 | 129 | 399 | 0 | 24 |
| corner right, first toll | 2178 | 1749 | 1830 | 2040 | 2208 | 114 | 114 | 960 | 1062 | **0** | 0 | 30 | 0 |
| corner right, second toll | 1086 | 696 | 735 | 1470 | 948 | **0** | 0 | 1602 | 1380 | 0 | 0 | 1920 | 1416 |
| corner right, third toll | 2298 | 1881 | 2022 | 3852 | **0** | 516x | 597 | 1200 | 0 | 0x | 0 | 0x | 756 |
| corner left, Sentence | 750 | 366 | 882 | **0** | 519 | 306 | 846 | 0 | 24 | 258 | 798 | 0 | 0 |
| corner left, first toll | 1464 | 1263 | 1452 | 78 | 1872 | 84 | 84 | 204 | 2556 | **0** | 0 | 198 | 2034 |
| corner left, second toll | 2394 | 1632 | 1974 | 2532 | 1764 | **0** | 0 | 1794 | 558 | 0 | 0 | 174 | 0 |
| corner left, third toll | 4596 | 3762 | 4044 | **0** | 3852 | 1032x | 1194 | 0 | 666 | 0x | 0 | 0x | 0 |

Bold is the variant chosen. Positive yaw swings the blade toward the king's left; positive raise
lifts its tip.

Three tests pin this in `client/src/player/mobs/king/choreography_tests.rs`:
- `a_planted_blow_beside_terrain_keeps_every_vertex_out_of_it_with_the_authored_hands`. In all four
  scenes, for every planted blow, the authored stroke clips and the presented one has zero vertices
  in terrain in each of the 63 samples. In every sample:
  - the grip is where the authored stroke puts it;
  - both hands hold the weapon;
  - no segment other than the blade and arms moves.
- `away_from_terrain_every_planted_blow_is_the_authored_pose`. On open floor, with no terrain, and
  with a monolith 4.1 blocks ahead, every sample is equal to the authored pose.
- `a_blade_variant_is_chosen_once_per_blow_and_again_for_a_new_blow_or_stance`.

## GPU captures

The part 3b chamber capture now also records the four planted blows 1.1 blocks from the chamber's
east wall, at the same four samples as beside the monolith. From `<repo-root>/client`:

```sh
WGPU_BACKEND=vulkan cargo test --locked \
    player::mobs::arena_capture::capture_bosses_in_the_shipped_chamber -- --ignored --exact
```

The adapter, pipeline, lighting and fixtures are part 3b's. The capture ran twice on the same
adapter:
- **after**, this change;
- **before**, the same tree with `choose_blade` returning at once, the authored stroke everywhere.

The before run reproduced part 3b's monolith counts exactly.

| Frames (preparation / mid release / last tick / recovery) | Before | After |
| --- | --- | --- |
| Monolith, sentence | 0 / 72 / 0 / 0 | 0 / 0 / 0 / 0 |
| Monolith, first toll | 0 / 72 / 0 / 60 | 0 / 0 / 0 / 0 |
| Monolith, second toll | 0 / 54 / 24 / 72 | 0 / 0 / 0 / 0 |
| Monolith, third toll | 60 / 72 / 96 / 72 | 0 / 0 / 0 / 0 |
| Wall, sentence | 0 / 72 / 0 / 0 | 0 / 0 / 0 / 0 |
| Wall, first toll | 36 / 72 / 54 / 60 | 0 / 0 / 0 / 0 |
| Wall, second toll | 42 / 60 / 42 / 72 | 0 / 0 / 0 / 0 |
| Wall, third toll | 60 / 72 / 96 / 72 | 0 / 0 / 0 / 0 |

Every other Draugr frame measured 0 in both runs. The below-floor depths are unchanged, including
the Sentence's 0.040 with its blade planted.

**Pixel comparison.** The two runs' PNGs were compared channel by channel.
- **Identical (27 of the 54 Draugr frames):** entrance, idle, gait, every cast and channel, the
  final stage and death, plus the Sentence frames in which the blade is upright or planted.
- **Changed (27):** the planted-blow frames beside terrain. The Sentence preparation beside the
  monolith differs by one pixel: at that sample its blade is not exactly upright.

**Sheets.**
- Monolith: [before](bosses-in-motion-1037/draugr-blows.png) (part 3b's sheet) and
  [after](blade-terrain-1103/monolith-after.png)
- Wall: [before](blade-terrain-1103/wall-before.png) and [after](blade-terrain-1103/wall-after.png)

A label names any clipping measured in its frame.

## Inspection record

The agent inspected the after sheets on 2026-09-11. This records what the frames contain. A capture
is not a claim about what a player perceives in play.

- **Unchanged in both scenes:** the Sentence preparation overhead and its planted blade at the
  last tick, the red contact boundaries, the pale strike strokes and the aggro marker.
- **The tolls' horizontal cuts** are presented as steep rising sweeps: the blade rises past the
  helm instead of crossing the monolith or the wall. The contact boundary and strokes still lie
  across the announced cone.
- **The third toll** beside the monolith is a level thrust swung past the column. Against the wall
  it is a low thrust with the tip toward the floor ahead of the wall.
- **The Sentence's mid-release blade** passes diagonally beside the monolith, and further to the
  side against the wall.
- **Not claimed:** that these larger departures read as the same blow at play distance. What stays
  the announced blow is the aim twist, the hands' trajectory and timing, and the regions and strokes
  the server's announcement draws.

## Remaining limits

- **One choice per stance.** Terrain that changes during a blow does not re-choose. The dungeon's
  chamber does not change.
- **No variant is guaranteed.** A king boxed in more tightly than these scenes gets the candidate
  with the fewest vertices in terrain, and the tests cover the four scenes above, not every stance.
- **The strike stroke is still drawn under terrain**, as part 3b records; this change does not
  touch the strike layer.
- **The corner** is measured by the unit test only: the chamber has no wall beside its monolith,
  so no captured scene combines them.
