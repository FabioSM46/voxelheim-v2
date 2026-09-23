# First dungeon descent: zone captures (#1298)

This record covers the visual evidence for the first dungeon's final acceptance. Every zone of the
route was captured with the production client renderer, from the production instance the server
sends a party. The end-to-end run and the instance memory measurement are recorded separately,
by the other pull request for #1298.

## What is captured

There are fourteen fixed views at 1280 × 720, the castle captures' resolution. Each one is in
[`dungeon-descent-1298-captures/`](dungeon-descent-1298-captures/), with a `.txt` manifest beside
it. The manifest records the view, the fixture state, seed, facing, worldgen, eye and target in
the drawing's frame, loaded chunks, sconces, active point lights, main draws and the adapter.
Interior views stand at the production `EYE_HEIGHT` above the zone's floor.

| View | State | What it shows |
| --- | --- | --- |
| `arrival_court` | drawn | The arrival slot facing the return portal's rune frame and veil |
| `draugr_hall` | drawn | The first hall, down its middle to the corridor, with the worn pillars and ceiling webs |
| `vargr_hall` | drawn | The second hall, the same way |
| `rune_hall` | drawn | The four stones and the inscription over them (the seed's order, one to four lit runes), either side of the shut door |
| `guardian_arena` | opened | The Vargr guardian's arena from its door, with the monoliths |
| `chasm` | opened | Down the opened trapdoor into the shaft |
| `pool_shore` | opened | The pool chamber from the shore |
| `cave_cavern` | opened | The cavern from the tunnel off the shore |
| `web_curtain` | drawn | The neck and its web curtain from the cavern |
| `gallery_grille` | drawn | The gallery toward the shut grille and the sconce by it |
| `sand_hall` | opened | The sand hall's dunes and pillars from its south end |
| `sand_twin_lever` | drawn | Across the hall at the west twin lever |
| `king_arena` | opened | The Draugr king's arena from the foot of the stair |
| `return_shortcut` | opened | The return shortcut's first flight, climbing from the king's arena |

**Nothing is brightened for the camera.** The lower zones are graded by the production
`cave_light` from the instance's world id. They are lit only by the seven wall sconces
`world.InstanceStaticProps` places, drawn through `castle_lighting`'s bounded pool from a snapshot
the server encoded. So the cave views are dark, as the cave is designed to be: `pool_shore`,
`cave_cavern` and `web_curtain` show little beyond the nearest sconce's pool of light. The upper
halls are roofed interiors under the world's ambient term. Legibility at night or on another
display was not judged, and no claim is made about it.

"Drawn" is the dungeon as a party first finds it: doors shut, trapdoor shut, levers down and
runes dark. "Opened" has every door and the guardian's trapdoor opened through the gate's own
`Update`, which is the state of a party that has solved everything. Only the doors and the
trapdoor change; no mechanism is shown pulled.

## Source of truth

- **Server export.** `server/internal/world/dungeon_capture_test.go` exports the production
  gated instance cache, the chunks `world.NewGatedInstanceCache` hands a session, including the
  halo of void around the shell as loaded air. The **VHDUNG01** layout is documented beside the
  exporter. A non-GPU test pins the export to the ungated generator outside the gate's cells,
  checks that every door cell is open in the opened state, and checks that the export is
  deterministic.
- **Sconce snapshot.** `server/internal/game/dungeon_capture_props_test.go` writes the sconces
  through the production static-prop index and `protocol.EncodeEntitySnapshot`.
- **Client capture.** `client/src/player/dungeon_capture.rs` runs the castle capture's harness
  with the same `WorldPlugin`, mesher, sky, AcesFitted tonemapping, field of view, readiness
  wait and one second of idle warm-up. `dungeon_capture_fixture.rs` validates the fixture before
  allocating, and its unit tests run without a GPU.

Camera ids are in the unrotated drawing's frame, so a capture needs an unturned dungeon. Seed 0
is one, and it is the seed used here.

## The boss arena capture

`client/src/player/mobs/arena_capture.rs` used to parse an ASCII drawing out of
`schematic_instance.go`. That drawing is now built with section helpers, so the parser panicked.
Its three opt-in tests now read a drawn-state fixture instead. Both shipped arenas are cut out of
the production dungeon and placed in the frame the review always used:

- the guardian's arena centred on (16.5, 14.5), and the king's on (16.5, 52.5);
- the floor top on y = 1 and the ceiling course on y = 9;
- each arena turned half round, which is a rotation and not a mirror, so its size, monoliths and
  walls are the dungeon's own.

The gallery that used to join the two arenas no longer exists, and it is not drawn back in. The
two far views that looked down it from 25 blocks are therefore 16-block views from inside each
arena, set off the diagonal so a monolith does not block the sight line; their names end in `-16`.
All three tests were run on this branch:

- `capture_bosses_in_the_shipped_chamber` wrote 93 PNGs and its clipping CSV, with no vertex
  inside a solid voxel;
- `measure_rendering_cost_in_the_shipped_chamber` passed;
- `measure_a_spider_horde_in_the_shipped_chamber` passed.

None of those outputs is committed.

## Machine

AMD Radeon RX 9070 XT (RADV GFX1201), radv, Mesa 26.2.3, Vulkan. The fixtures and the images
come from source commit `57da289` on this branch, which was clean at capture time. The PNGs were
re-encoded losslessly, from RGBA to RGB with PNG optimisation, to 4.2 MB in total.

## Reproduce

From `<worktree>/server`, for each state:

```sh
export DUNGEON_CAPTURE_SOURCE_COMMIT="$(git rev-parse HEAD)"
DUNGEON_CAPTURE_FIXTURE=<output-directory>/dungeon-opened.vhd DUNGEON_CAPTURE_STATE=opened \
  go test ./internal/world -run '^TestExportDungeonCaptureFixture$' -count=1
DUNGEON_CAPTURE_FIXTURE=<output-directory>/dungeon-opened.vhd \
  go test ./internal/game -run '^TestExportDungeonCaptureSnapshot$' -count=1
```

Repeat these with `drawn` in place of `opened`. Then, from `<worktree>/client`, run once per view
with a fresh output path, pairing each view with the fixture state the table names:

```sh
DUNGEON_CAPTURE_FIXTURE=<output-directory>/dungeon-drawn.vhd \
DUNGEON_CAPTURE_SNAPSHOT=<output-directory>/dungeon-drawn.vhd.snapshot \
DUNGEON_CAPTURE_VIEW=rune_hall DUNGEON_CAPTURE_OUTPUT=<output-directory>/rune_hall.png \
WGPU_BACKEND=vulkan cargo test --locked --bin voxelheim-client capture_dungeon_production_zone \
  -- --ignored --exact player::castle_capture::dungeon::capture_dungeon_production_zone
```

For the boss arena review, pass the drawn fixture:

```sh
DUNGEON_CAPTURE_FIXTURE=<output-directory>/dungeon-drawn.vhd WGPU_BACKEND=vulkan \
  cargo test --locked --bin voxelheim-client capture_bosses_in_the_shipped_chamber -- --ignored
```

A view whose state does not match the fixture is refused. `DUNGEON_CAPTURE_TICK` selects the
frozen sky tick; the default is 6000.
