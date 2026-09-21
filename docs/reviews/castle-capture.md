# Castle architecture capture and acceptance — issue #1202

For worldgen 37 furnishing/lighting results see [combined acceptance](castle-lighting-acceptance.md).
This architecture baseline predates that dressing.

The final architecture remains **63 × 63 × 68 blocks**, worldgen **36**. Its
floor plan, furniture reservations and exact standing-height exception for the
E7 audience dais are in [CASTLE_PLAN.md](../CASTLE_PLAN.md). The nine main floors,
bridge, complete two-wide curtain circuit, four corner lookout rooms and four
spire interiors are connected to the gate. No expansion, city-spacing change,
plateau extension or ward-radius change is needed.

This delivery adds one reusable, opt-in **production client** capture harness.
It uses `WorldPlugin`, the production neighbour-aware terrain mesher, material
palette, camera FOV, AcesFitted tonemapping and sky systems. There is no duplicated
ASCII schematic reader or cube-per-block renderer. Furniture and lighting
follow-ups extend this same scene with their production snapshot consumers.
The architecture evidence has the current production lighting: dark interiors
are not brightened for screenshots, and opaque placeholder tower glazing is not
presented as an already completed grille window.

## What is captured

`exterior_gate.png` includes the complete differentiated spire silhouette;
`gate_entry.png` shows the approachable gate and court. `west_stair.png`,
`east_stair.png`, `bridge.png`, `throne.png` and the four `*_lookout.png` views
show the actual interior geometry. The fixed camera IDs and canonical poses
live in `client/src/player/castle_capture.rs`; player-height views use the
production `EYE_HEIGHT`, not a raised inspection camera. The exterior uses an
explicit free camera to include the whole silhouette.

| Walkthrough | Actual upper standing height | Authoritative trace filename |
| --- | --- | --- |
| `walk-nw.mp4` | 35 | `TestCastleWesternSpireLookoutsAreReachedByWalking_tower10_lane0_rotation0.tsv` |
| `walk-sw.mp4` | 29 | `TestCastleWesternSpireLookoutsAreReachedByWalking_tower20_lane0_rotation0.tsv` |
| `walk-ne.mp4` | 41 | `TestCastleEasternSpireLookoutsAreReachedByWalking_tower50_lane0_rotation0.tsv` |
| `walk-se.mp4` | 35 | `TestCastleEasternSpireLookoutsAreReachedByWalking_tower42_lane0_rotation0.tsv` |
| `walk-curtain.mp4` | 13 | `TestCastleCourtStairAndCurtainCircuitNeedNoJump_lane0_rotation0.tsv` |

Each trace starts at canonical gate feet `(31.5, 0, 62.5)`, walks the real
flights, reaches its lookout or all four corner lookout rooms, then returns
to the gate. The trace records the positions after actual `Player.step` calls
at 20 Hz: ordinary intent, gravity, collision and fall damage remain active.
It applies no jump input or position/vertical-velocity correction. Both tread
lanes and all four building rotations remain covered by the authoritative
movement tests; the five videos use the actual capital's facing zero.

The camera samples every second trace tick and always includes the exact terminal
pose. Regular frames are 100 ms apart; an odd terminal interval is 50 ms. Encoding
at 10 fps holds the final pose for a frame, rather than altering simulation time.
The manifest records trace endpoints, highest feet, frame counts, elapsed capture
time, source commits and SHA-256 hashes of fixtures, traces and output artifacts.

## Export and coordinate contract

`server/internal/world/castle_capture_test.go` exports the actual keep selected
by `CapitalAt(seed)`. The default seed is `0x5eed`. The **VHCAST03** binary header
is 96 bytes, little endian:

| Offset | Field |
| --- | --- |
| 0 | Eight-byte magic `VHCAST03` |
| 8, 12 | Format 3 and worldgen version, u32 |
| 16 | Seed, i64 |
| 24, 28, 32 | Actual building facing, explicit review turn, scene mode; u32 |
| 36 | Actual building origin XYZ, three i64 |
| 60 | Exported voxel-volume origin XYZ, three i64 |
| 84 | Volume dimensions XYZ, three u32 |
| 96 | Exact u16 block payload, X fastest, then Z, then Y |

Mode 1 exports a bounded 256-block-wide neighbourhood with actual `Generate`
results, including its terrain and other buildings. Its vertical range encloses
the castle and nearby ground. Every intersecting chunk is inserted into the
client store, **including loaded all-air chunks**, so production rays do not
confuse loaded sky with missing terrain. Empty chunks correctly create no mesh.
A byte-for-byte determinism test and independent castle/exterior chunk comparisons
pin the export to actual generation.

Mode 0 exports the isolated placed schematic and one flat supporting layer.
The parser checks dimensions, total volume (at most 16 million voxels), coordinate
overflow, castle containment, exact length and block IDs before payload allocation.
It consumes the production palette, including AIR and grille IDs 58/59.

The actual keep faces zero. `review_turn=1..3` is explicitly synthetic review
metadata and is accepted only for the isolated mode. The server rotates both
voxel coordinates and directional block IDs before insertion into `ChunkStore`;
the camera uses the same continuous quarter-turn transform. The runtime capital
orientation never changes. The actual building origin remains separate from the
export volume origin, so later authoritative static-prop origins are not relocated
independently. Any later review-turn prop export must transform origins and
facings together using this same frame.

## Timing, GPU readiness and counters

Only `SkyClock` is frozen, through a test-only hook that freezes both solar and
lunar phase; ordinary snapshot anchoring restores normal advancement. GPU/mesher
readiness uses zero elapsed time, at least 200 update frames and ten consecutive
drained queue/in-flight observations. Once ready, production timers receive
exactly **20 × 50 ms of idle warmup**. A walkthrough already has its initial
camera at the first authoritative pose during this warmup.

Each captured pose advances Bevy `Time` once by its actual trace interval.
Additional updates waiting for GPU readback advance it by zero. This keeps later
5 Hz light selection and flame animation following the route while the sky stays
fixed, without resetting lights or changing ambient exposure per frame. Regression
tests cover this elapsed-time contract and loaded empty chunks.

The harness disables Winit and pipelined rendering, targets an RGBA8 sRGB image,
waits for GPU completion, observes `ScreenshotCaptured`, and decodes each PNG
before reporting success. The calibrated counters wrap actual mesh draw commands
for main, shadow and prepass submissions. A multidraw is one API submission;
fullscreen passes are excluded. Counts are labelled as baseline-view submissions,
not total game draw calls or measured full-city FPS. The manifests record adapter,
driver and backend without machine bus identifiers. A bounded generated region
is not a full streamed-city performance benchmark. Any future timing around
`app.update()` plus GPU completion is synchronized end-to-end frame time, not
GPU-only timestamps.

## Reproduce the artifacts

Use a fresh absolute `<output-directory>` outside tracked files. Run exports
from `server/` on a committed source tree:

```sh
export CASTLE_CAPTURE_FIXTURE="<output-directory>/capital.vhc"
export CASTLE_CAPTURE_SOURCE_COMMIT="$(git rev-parse HEAD)"
CASTLE_CAPTURE_MODE=generated go test ./internal/world -run '^TestExportCastleCaptureFixture$' -count=1
CASTLE_CAPTURE_TRACE_DIR="<output-directory>/traces" go test ./internal/game -run 'TestCastle(Western|Eastern)SpireLookoutsAreReachedByWalking|TestCastleCourtStairAndCurtainCircuitNeedNoJump' -count=1
```

The fixture's `.vhc.json` sidecar records its source commit and SHA-256. For each
synthetic rotation export a separate `review-turn-N.vhc` with mode `isolated` and
`CASTLE_CAPTURE_REVIEW_TURN=N`. Do not pass a review turn to generated mode.

From `client/`, use the same committed source and an actual GPU. Ordinary CI keeps
this test ignored; all parser, time and movement regressions run without a GPU.

```sh
CASTLE_CAPTURE_FIXTURE="<output-directory>/capital.vhc" \
CASTLE_CAPTURE_OUTPUT="<output-directory>/exterior_gate.png" \
WGPU_BACKEND=vulkan CARGO_BUILD_JOBS=2 \
cargo test --workspace --locked capture_castle_production_scene -- --ignored --nocapture
```

Set `CASTLE_CAPTURE_VIEW` to any fixed ID listed above, and choose a matching
fresh output basename. The default frozen tick is 6000; `CASTLE_CAPTURE_TICK`
selects another exact time without changing the production sky model. For the
three isolated rotation images, choose their matching `.vhc` fixtures.

For a walkthrough additionally set `CASTLE_CAPTURE_TRACE` to the table's TSV
under `<output-directory>/traces`, and set output to `walk-nw.png` (or the matching
route name). The harness writes `walk-nw.frames/frame-00000.png` onward and a
`walk-nw.txt` report. Encode the completed frame sequence:

```sh
ffmpeg -framerate 10 -i "<output-directory>/walk-nw.frames/frame-%05d.png" \
  -c:v libx264 -threads 2 -pix_fmt yuv420p -crf 23 -movflags +faststart \
  "<output-directory>/walk-nw.mp4"
python3 scripts/castle-capture-manifest.py "<output-directory>"
```

Run the final Python command from the repository root. It requires ffprobe for
video verification and rejects dirty capture sources, different fixture/capture
commits, digest mismatches, missing frames, malformed traces or elapsed-time
mismatches. `manifest.json` contains basenames and hashes, never checkout paths.
Artifacts remain local review outputs; bulky binaries are not committed.

## Final generation cost

Before is source `68b182b`, before all five architecture deliveries. After is
worldgen 36 with the completed geometry. Both use this command, five repetitions
of twenty iterations, with an unchanged open-country control:

```sh
GOMAXPROCS=2 go test ./internal/world -run '^$' -bench 'BenchmarkGenerateIn(ACapital|OpenCountry)$' -benchtime=20x -count=5
```

| Benchmark | Before ns/op | After ns/op |
| --- | --- | --- |
| Capital | 6,520,047 / 6,648,099 / 5,835,833 / 7,322,037 / 5,865,848 | 6,633,499 / 6,538,815 / 9,440,849 / 8,107,810 / 9,099,361 |
| Open country | 7,094,132 / 6,627,444 / 7,139,014 / 6,762,859 / 6,761,931 | 10,228,643 / 9,994,528 / 10,029,857 / 10,286,784 / 10,179,433 |

Capital medians are 6.520 ms before and 8.108 ms after; the unrelated control
moves from 6.763 ms to 10.179 ms. Concurrent compilation affected these samples,
so they report an observed cost smoke check, not an isolated architectural
regression or speedup. The final acceptance delivery changes no generation rule,
protocol or worldgen version; the geometry deliveries already moved worldgen
31 through 36 and updated the applicable golden/version expectations.

## Furnished snapshot and matched cost capture

The optional game exporter reads the same `.vhc` fixture and its source/digest
sidecar, obtains the actual capital catalogue through the production static-prop
index, and encodes an `EntitySnapshot`. The client capture decodes that envelope
and feeds the production snapshot buffer and static-prop systems. Review turns
rotate descriptor cell origins and facing together with the exported architecture.
No duplicate rendering catalogue is maintained by the capture.

After exporting the voxel fixture, use the same source commit to export descriptors:

```bash
CASTLE_CAPTURE_FIXTURE=<output>/capital.vhc \
CASTLE_CAPTURE_SOURCE_COMMIT=$(git rev-parse HEAD) \
go -C server test ./internal/game -run '^TestExportCastleCaptureSnapshot$' -count=1
```

Pass `CASTLE_CAPTURE_SNAPSHOT=<output>/capital.vhc.snapshot` to the client capture.
`CASTLE_CAPTURE_FURNITURE=off` clears only the decoded descriptor set for the matched
empty-room baseline. The default consumes every exported descriptor. Fixed views
include `banquet`, `throne`, `study` and all four tower lookouts. The manifest binds
the snapshot digest and source commit to the voxel fixture and capture source.

Each report records 120 warmed frames with a GPU wait after each update, reporting
p50 and p95 synchronized CPU+GPU frame latency, entity count, static-prop root count,
and active/shadowed point-light counts. This is scoped capture latency, not GPU-only
timing or full-game FPS. Mesh draw submissions retain their main/shadow/prepass
scope; they are not inferred from entity counts.
