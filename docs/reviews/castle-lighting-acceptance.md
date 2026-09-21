# Furnished castle acceptance — #1203 / #1204

Worldgen 37 completes the existing 63 × 63 × 68 castle with **209 roots**:
117 furnishings and 92 cosmetic fixtures, below the shared 256-root limit.
The [room inventory](castle-furniture-placement.md) covers banquet, throne,
kitchen, council, guard room, library, galleries and four furnished tower rooms.
Fixtures comprise 82 wall sconces, seven standing candelabra at landings and
three tabletop candelabra (two banquet, one council). Slots 253–256 remain unused.

Eighteen main-wing windows have 3 × 5 openings through both wall layers, with
one grille layer. NW/SW/SE tower openings face south; NE faces north to avoid
its wing roof. Exterior slate backing is cleared too. Sills, headers, roofs,
piers and stair guards remain intact.

Tests cover actual furnished movement in four rotations and both lanes, fixture
mount/support/overlap, full-depth apertures and unchanged authority/lifecycle.
The production precipitation probe confirms shelter in all nine main rooms in
every review rotation. Only exact barred tower reveals are excluded from room
connectivity; they are window sills, not rooms.

## Sources and settings

32 stills and seven videos use `5706ab3`; extra west-wing/bridge videos use
`0bdc373` (route tests only). Client source, generated fixture and encoded snapshot
bytes match. The production decoder consumes that snapshot. Manifests verify clean
source, hashes, traces and frames. Main views use generated surroundings; window
pairs use labelled isolated rotations. Poses live in `castle_capture.rs`.

1280 × 720; default FOV; EV100 9.7; AcesFitted; clear weather; day/night ticks
6000/18000. Adapter: AMD Radeon RX 9070 XT (RADV GFX1201), Vulkan, Mesa 26.2.3;
8192 texture-array layers. Directional shadows: two cascades, 64 blocks, 2048 map.
Point shadows: 512 pixels per face.

The [pool](castle-candle-pool.md) caps shadowed lights at four, with device/range
limits and bounded visibility ranking. Unselected flames remain visible. Its
intensity tuning and unchanged campfire/portal behavior are documented there.
GPU comparison found that the sky dome occluded sunlight after shadows were
enabled. `NotShadowCaster` on celestial meshes fixes this, with a regression test;
solar motion, ambient curves and weather remain unchanged.

## Window comparison

Candles are disabled. Only the grille-to-stone diagnostic switch changes within
each pair; camera, time and exposure match. All four rotations show floor patches
with bar shadows which disappear when blocked. Rectangles are `(left, top, right,
bottom)`, exclusive right/bottom. Means use sRGB linearization then Rec.709
`0.2126 R + 0.7152 G + 0.0722 B`: final-image luminance, not physical lux.

| Turn | View / tick | Floor rectangle | Open | Blocked | Ratio |
| --- | --- | --- | --- | --- | --- |
| 0 | window_patch / 12000 | 800,510,1100,550 | 0.036301 | 0.003046 | 11.92 |
| 1 | window_east / 6000 | 560,485,700,510 | 0.056637 | 0.002905 | 19.50 |
| 2 | window_patch / 0 | 480,580,650,640 | 0.057306 | 0.003046 | 18.81 |
| 3 | window_patch / 6000 | 530,495,730,520 | 0.059765 | 0.002979 | 20.06 |

Each pair measures the same interior floor. Sun-facing windows/times are chosen
per rotation; direct sun is not promised through every window at every hour.

## Rendering cost

At tick 6000, `lighting=off` disables candle models/lights and directional shadows;
`furniture=off` removes all prop descriptors. These compare furnishings-only and
complete lighting costs, not point-shadow cost alone.

| View | Lighting / furnishings | Live entities | Main / shadow draws | Active / shadowed points | p50 / p95 ms |
| --- | --- | --- | --- | --- | --- |
| Banquet | off / off | 517 | 4 / 0 | 0 / 0 | 4.394 / 13.903 |
| Banquet | off / on | 1100 | 5 / 0 | 0 / 0 | 3.958 / 13.100 |
| Banquet | on / off | 520 | 4 / 5 | 0 / 0 | 4.912 / 13.580 |
| Banquet | on / on | 1403 | 6 / 79 | 4 / 4 | 15.162 / 25.161 |
| Exterior gate | off / off | 517 | 4 / 0 | 0 / 0 | 4.496 / 14.805 |
| Exterior gate | off / on | 1100 | 5 / 0 | 0 / 0 | 4.447 / 13.865 |
| Exterior gate | on / off | 520 | 4 / 4 | 0 / 0 | 4.940 / 15.562 |
| Exterior gate | on / on | 1399 | 6 / 6 | 0 / 0 | 5.122 / 17.004 |

Prepass draws are zero. Furnishings-on retains 209 roots; cosmetic details use the
existing 64-block visibility rule. Entities count live main-world entities via
`World::entity_count`, not allocated indices. Draws are actual mesh API submissions
after batching (one per multidraw), excluding fullscreen passes. Timings contain
120 warmed updates synchronized with GPU completion: CPU+GPU latency of a bounded
scene, not GPU-only time or full-game FPS. Small baseline differences are noise.
Eight lights were tried and reduced to four after profiling the shadow workload.

## Night walks

Matched day/night views cover gate, banquet, throne, study, corridor, landing,
NW lookout and tower study; eight cost views and eight window images complete
32 stills. Nine night videos include the exact return to the gate. Traces have distinct wing/lane names and are never joined or edited.

| Route | Trace ticks | Video frames | Highest feet Y |
| --- | --- | --- | --- |
| East main floors, second lane | 2321 | 1161 | 28 |
| West main floors, first lane | 1768 | 885 | 21 |
| Curtain circuit | 1706 | 854 | 13 |
| NW spire | 1628 | 815 | 35 |
| SW spire | 1147 | 574 | 29 |
| NE spire | 1837 | 919 | 41 |
| SE spire | 1432 | 717 | 35 |
| Audience dais | 692 | 347 | 8 |
| Bridge, outward and return | 1184 | 593 | 21 |

The 6,865 frames include stair approaches, intermediate landings, curtain corners,
room entrances and tower arrivals. Inspected next steps and doorway edges remain
readable; recesses remain dark. Table settings, legs, bookcases, rugs, carved seat
and candle mounts are recognizable in the fixed player-height views.

## Reproduction

Use [the harness instructions](castle-capture.md), committed source and a dedicated
Cargo target per worktree. Export traces with `CASTLE_CAPTURE_TRACE_DIR` and filter
`TestCastle(MainFloors|CourtStair|WesternSpire|EasternSpire|AudienceDais|Bridge)`;
replay complete TSVs at tick 18000 and encode at 10 fps. Cost pairs use the table's
views/settings and `CASTLE_CAPTURE_LIGHTING`/`CASTLE_CAPTURE_FURNITURE` switches.
For windows, export isolated fixtures with `CASTLE_CAPTURE_REVIEW_TURN`, use the
window table's view/tick, `CASTLE_CAPTURE_CANDLES=off` and
`CASTLE_CAPTURE_WINDOWS=open/blocked`. Validate fresh output directories with the
manifest script. Bulky images, videos and fixtures remain local review artifacts.
