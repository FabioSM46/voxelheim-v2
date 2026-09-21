# Furnished castle and lighting acceptance — #1203 / #1204

This completes the furnishing and lighting deliveries on the existing 63 × 63 × 68
castle. Worldgen 37 opens the windows and places 92 cosmetic fixtures beside the
117 authoritative furnishing roots: **209 roots**, below the shared 256-root cap.
Protocol 45 and its refusal of older peers were delivered with the catalogue.

## Placement and circulation

The [room inventory](castle-furniture-placement.md) covers the two fully set banquet
tables, carved throne and runner, kitchen/pantry, council, guard room, library,
upper galleries and four scholar/lookout rooms. The fixture catalogue adds 82 wall
sconces, seven standing candelabra at upper landings and three tabletop candelabra
(two banquet tables and the council table). Slots 253–256 remain unused.

Eighteen main-wing openings are three blocks wide and five high, with one layer
of narrow grille members through the complete two-block wall. NW, SW and SE
lookouts have south openings; the NE opening faces north because its former south
slit looks into the intact wing roof. Exterior slate trim is cleared as well as
the glass: a grille backed by opaque trim was found and corrected in GPU review.
Sills, headers, structural piers, roofs and stair guards remain in place.

Authoritative walking tests use the complete catalogue, all four rotations and
both tread lanes across the nine main floors, bridge, curtain, dais and towers.
They apply ordinary movement without jump or teleport corrections and check
floor-candle envelopes at every tick. Rotated fixture tests verify mounts,
support and separation from architecture/furniture. Existing authority, arrival,
respawn, ward and snapshot-lifecycle regressions remain green.

The aperture test checks full wall depth and outward rays. It runs on all platforms;
the former `_windows_test.go` name selected Windows only. Connectivity excludes
only exact two-high tower reveals with actual grilles. Production precipitation
shelter probes pass in all nine main rooms in each review rotation.

## Render settings and provenance

The 32 still captures and seven initial videos use source `5706ab3`; the extra
west-wing and bridge videos use `0bdc373`, which changes only route tests.
The client tree is identical, and the generated fixture and encoded snapshot are
byte-for-byte identical between these sources. All use real server-exported
geometry and `EntitySnapshot`, decoded and consumed by the production client. The
fixture/snapshot digests, clean source, complete traces and output hashes are
verified by `scripts/castle-capture-manifest.py`. Each review turn has its own
labelled isolated fixture; the main views and walks use the generated capital
neighbourhood. Fixed player-height cameras live in `castle_capture.rs`.

Settings: 1280 × 720, default field of view, EV100 9.7, AcesFitted, clear weather,
day tick 6000 / night tick 18000, AMD Radeon RX 9070 XT (RADV GFX1201), Vulkan,
Mesa 26.2.3. The adapter exposes 8192 texture-array layers. Directional shadows
use two cascades over 64 blocks and a 2048 map; point shadow faces use 512 pixels.

The [bounded pool](castle-candle-pool.md) selects at most four shadowed candles
within 24 blocks, limited further by the device and other point shadows. Ranking
uses at most 32 sight rays at 5 Hz with stable IDs and retention. Unselected flames
remain visible. Wall/floor/table output is 120,000/240,000/160,000 in Bevy's lumen
scale: game-art tuning for production exposure, not calibrated physical candles.
Existing campfires and portals disable shadows and retain their behavior.

GPU comparison exposed a previously invisible interaction: the sky dome itself
cast directional shadows after shadows were enabled. `NotShadowCaster` on the
four celestial meshes fixes that occluder; a regression verifies their exclusions.
The sun trajectory, daylight/ambient curves and weather behavior are unchanged.

## Window comparison

Candles are disabled for these pairs. Only the grille-to-stone diagnostic switch
changes between each open and blocked capture; camera, exposure and solar time
remain identical. All four rotations produce a sunlit floor patch with visible
bar shadows; blocking the openings removes it.

Pixel rectangles use `(left, top, right, bottom)` with exclusive right/bottom.
Mean luminance is computed after sRGB linearization with Rec.709 weights
`0.2126 R + 0.7152 G + 0.0722 B`. It measures the final tone-mapped image, not lux.
Every rectangle lies on the same interior floor in its matched pair.

| Review turn | View / tick | Floor rectangle | Open | Blocked | Ratio |
| --- | --- | --- | --- | --- | --- |
| 0 | window_patch / 12000 | 800,510,1100,550 | 0.036301 | 0.003046 | 11.92 |
| 1 | window_east / 6000 | 560,485,700,510 | 0.056637 | 0.002905 | 19.50 |
| 2 | window_patch / 0 | 480,580,650,640 | 0.057306 | 0.003046 | 18.81 |
| 3 | window_patch / 6000 | 530,495,730,520 | 0.059765 | 0.002979 | 20.06 |

This chooses a sun-facing window/time for each orientation; it does not promise
direct sun through every window at every hour.

## Matched rendering cost

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

Prepass mesh draws were zero in all eight measurements. Both views retain the
same 209 roots when furnishings are on; cosmetic detail visibility is bounded by
the existing 64-block distance rule. Small baseline timing differences are noise,
not evidence that adding furniture speeds rendering up.

`lighting=off` disables candle registration and directional shadows;
`furniture=off` removes all static-prop descriptors. The four combinations compare
furnishings-only and complete lighting costs at the same view and settings.
The lighting toggle also removes candle model children, so its difference is not
a pure point-shadow benchmark. Entity counts are
live main-world entities (`World::entity_count`), not allocator capacity. Draws
are measured API mesh submissions after batching, with multidraw counted once;
fullscreen passes and render-world entities are not included.

Frame distributions contain 120 warmed updates, each synchronized with GPU
completion. They measure CPU+GPU frame latency of this bounded capture scene,
not GPU-only time or full-game FPS. Eight lights were tried during tuning and
reduced to four after the shadow workload became dominant; the final table above
reports the actual chosen budget, without claiming a portable FPS target.

## Walks and visual evidence

Matched day/night images cover gate entry, banquet, throne, study, corridor,
stair landing, NW lookout and tower study. The fixed views show table legs,
place settings, bookcases, carved seat, floor runners, candle mounts and local
shadows at player height. The eight window images complete 32 still captures.

Nine complete night videos include the exact return to the gate. Distinct wing
and lane export names prevent one walk replacing another; the old main-floor
export represented the east wing's second lane. A separate test crosses both
bridge lanes in all rotations. Traces are never joined or edited.

| Night route | Trace ticks | Encoded frames | Highest feet Y |
| --- | --- | --- | --- |
| East main floors, second lane | 2321 | 1161 | 28 |
| West main floors, first lane | 1768 | 885 | 21 |
| Complete curtain circuit | 1706 | 854 | 13 |
| Northwest spire | 1628 | 815 | 35 |
| Southwest spire | 1147 | 574 | 29 |
| Northeast spire | 1837 | 919 | 41 |
| Southeast spire | 1432 | 717 | 35 |
| Audience dais | 692 | 347 | 8 |
| Bridge, outward and return | 1184 | 593 | 21 |

The videos contain 6,865 frames in total. Inspection includes the gate, stair
approaches, intermediate landings, curtain corners, room entrances and tower
arrivals. Next steps and doorway edges remain readable; recesses stay dark.
Solid walls retain shadow separation, rather than receiving globally raised
ambient illumination. Movement and overlap assertions run at every server tick; separate placement
checks verify support, independently of visual inspection.

## Reproduction

Use a committed checkout and a dedicated Cargo target directory per worktree.
Sharing a target between worktrees can incorrectly reuse a same-named local test
binary. Build the capture executable from the same source as the fixture.

Follow [the capture harness instructions](castle-capture.md) for fixture/snapshot
exports and manifest validation. For all furnished movement traces, export with
`CASTLE_CAPTURE_TRACE_DIR` and run the game tests matching
`TestCastle(MainFloors|CourtStair|WesternSpire|EasternSpire|AudienceDais|Bridge)`.
For the nine rotation-zero route videos set `CASTLE_CAPTURE_TRACE` to the complete
TSV and `CASTLE_CAPTURE_TICK=18000`; encode its frame directory at 10 fps as documented.

For cost pairs use `banquet` and `exterior_gate` at tick 6000 and the four
`CASTLE_CAPTURE_LIGHTING=off/on`, `CASTLE_CAPTURE_FURNITURE=off/on` combinations.
For each window pair export `CASTLE_CAPTURE_MODE=isolated` with its
`CASTLE_CAPTURE_REVIEW_TURN`, choose the table's view/tick, set
`CASTLE_CAPTURE_CANDLES=off`, and capture `CASTLE_CAPTURE_WINDOWS=open/blocked`.
Keep output basenames and directories fresh. Large PNG/MP4/fixture outputs remain
local review artifacts; the manifest binds each to its source and content hash.
