# First dungeon bosses in motion in the shipped chamber, part 3b (#1037)

[Part 3a](boss-strikes-1037.md) reviewed strike reach on a neutral floor with a reference box. This
part captures both final models in motion inside the dungeon's own chamber. It measures feet and
weapon clipping against that chamber's voxels, and corrects the one demonstrated issue that is
presentation-only: a Draugr corpse folding into terrain. It changes no schema, server code, audio,
region, timing or outcome.

## The chamber

`client/src/player/mobs/arena_capture.rs` reads the server's drawing,
`server/internal/world/schematic_instance.go`, at capture time. It maps the seven runes that
drawing uses (`_ b U K k v O`) to the client palette ids that `scripts/test/block-palette-parity.test.sh`
pins to the server's. It then stores the resulting 33 × 10 × 69 voxels as six chunks in the client's
`ChunkStore`, and draws them with `WorldPlugin`: the production chunk mesher and terrain material.
Before the first frame, the harness waits until every chunk holding voxels has a mesh.

What it is **not**:
- **Placement.** The layout is unrotated; the server rotates it per seed.
- **Gallery gate.** The gate the server places at runtime is not part of the drawing, so the
  gallery is open.
- **Lighting.** The frames are lit for review by an ambient term and an unshadowed directional
  light, because the drawing has a roof that would shadow everything. That is not the dungeon's
  lighting, and nothing here claims how the chamber looks in play.

## Capture

From `<repo-root>/client`:

```sh
WGPU_BACKEND=vulkan cargo test --locked \
    player::mobs::arena_capture::capture_bosses_in_the_shipped_chamber -- --ignored --exact
```

**How it renders.**
- **Adapter:** AMD Radeon RX 5700 XT (RADV NAVI10), Vulkan, Mesa 25.2.8. This names the adapter,
  not a frame-rate claim.
- **Pipeline:** the production snapshot consumer, animator, encounter pose pass, regalia, boundary
  cues, spell and strike layers and encounter readings, offscreen at 1280 × 720 and the default
  field of view.
- **Fixtures:** snapshots and timelines at 20 Hz, with the server catalogue's phase ticks and regions
  after part 2. They are placed the way the server places them: from the boss root, the announced
  facing and combination bearings. A charge's lane and a leap's landing disc stay where the move was
  announced while the snapshots carry the body on. The capture waits for each PNG on disk, not a frame count, and
  discards a warm-up frame taken before the first recorded one.

**What it captured.** 77 recorded frames.

| Boss | Scenes |
| --- | --- |
| Vargr, courtyard (39) | Idle at the anchor, and from 13 and 25 blocks. Gait toward a monolith and a turn to face it (4). Bite, single claw, left and right paired claw and jaws beside that monolith, each at preparation end, release midpoint and last tick, and recovery midpoint (20). Charge preparation, two run samples and the stop at the east wall (4). Leap preparation, airborne and landing over clear floor (3). Stage-two jaws preparation from 13 and 25 blocks (2). Death at 0, 250 and 1,000 ms (3). |
| Draugr, hall (38) | Entrance at 0, 650 and 750 ms (3), and idle from 13 and 25 blocks (2). Gait toward a monolith and a turn to face it (4). Sentence, first, second and third toll beside that monolith at the same four samples (16). Spear preparation and release, Burial preparation, Burial pulse contact, Edict pulse and Requiem pulse (6). The final-stage transition, the final stage, and the final stage from 13 and 25 blocks (4). Death at 0, 250 and 1,000 ms (3). |

The distance views are taken at a standing player's eye height, 1.7 blocks above the floor. They
look down the chamber's axis through the gallery openings.

**Sheets.**
- [Vargr motion](bosses-in-motion-1037/vargr-motion.png) and [Vargr blows](bosses-in-motion-1037/vargr-blows.png)
- [Draugr motion](bosses-in-motion-1037/draugr-motion.png) and [Draugr blows](bosses-in-motion-1037/draugr-blows.png)
- Distance: [Vargr](bosses-in-motion-1037/distance.png) and [Draugr](bosses-in-motion-1037/distance-king.png)
- [Corpse fall, before and after the correction](bosses-in-motion-1037/corpse-fall.png)

A label names any clipping measured in its frame.

## Clipping measurements

For every recorded frame the harness takes each vertex of the boss's visible meshes, in world space,
and records:
- how many lie strictly inside a solid chamber voxel, at least 0.02 blocks from every face;
- how deep the lowest lies below the floor top.

It does not attribute a vertex to a segment. The frames show which part it is.

| Frames | Vertices in terrain | Below the floor |
| --- | ---: | ---: |
| All 39 Vargr frames | 0 | 0 |
| Draugr entrance: start / 650 ms / 750 ms | 0 | 0.120 / 0.077 / 0.009 |
| Draugr idle from 13 and 25 blocks | 0 | 0.009 |
| Draugr sentence: preparation / mid release / last tick / recovery | 0 / 72 / 0 / 0 | 0 / 0.003 / 0.040 / 0.040 |
| Draugr first toll: preparation / mid release / last tick / recovery | 0 / 72 / 0 / 60 | 0 |
| Draugr second toll: preparation / mid release / last tick / recovery | 0 / 54 / 24 / 72 | 0 |
| Draugr third toll: preparation / mid release / last tick / recovery | 60 / 72 / 96 / 72 | 0 |
| Draugr Burial preparation and pulse, Requiem pulse | 0 | 0.040 |
| Draugr settled corpse, before the correction | 108 | 0 |
| Draugr settled corpse, after | 0 | 0 |
| Every other Draugr frame | 0 | 0 |

The Draugr's model has 2,928 vertices; the Vargr's has 1,296.

**Terrain.** In every Draugr frame with vertices in terrain, the king stands 1.1 blocks from a
monolith's face, facing it, and the frames show his blade entering the monolith. **The Vargr's
fangs, claws and paws never reached the monolith 0.55 blocks from its front edge, nor the wall
it charged into.**

**The floor.** The below-floor depths coincide with poses in which the frames show the blade planted
in the floor: the blade-supported entrance crouch, the Sentence's blade stuck at the end of its cut,
and the sword planted for Burial and Requiem. The design asks for those poses. The 0.009 block after
the entrance is not attributed further.

## Correction: a corpse no longer folds into terrain

The king's corpse always folded forward about his hips. Felled facing the monolith, it lay with
108 vertices inside it, as the [before frame](bosses-in-motion-1037/corpse-fall.png) shows.

**The fix.**
- `king::motion::Motion::choose_fall` decides the fall direction once, when a fall begins. If the
  fully folded body would lie inside solid terrain forward and not backward, the same fold is
  mirrored.
- `animate` supplies the terrain from the chunk store.
- The snapshot root and yaw never move, the model and its envelope are unchanged, and a replant
  chooses again.

`a_corpse_folds_away_from_terrain_it_would_otherwise_lie_in` pins four things:
- a wall ahead mirrors the fall;
- open or boxed-in ground keeps the authored forward fall;
- the choice does not change once made;
- the mirrored corpse rests on the floor.

After the correction, the settled corpse measured 0 vertices in terrain in the same scene.

## Inspection record

The agent inspected the sheets and single frames on 2026-09-10. This records what the frames
contain. A capture is not a claim about what a player perceives in play.

- **Both bosses read in the chamber.**
  - The Vargr's low silhouette and the Draugr's tall one stand out against the chamber's black brick
    and the arrival portal.
  - Encounter readings, telegraph outlines, contact boundaries and the strike, spell and ritual layers
    all draw on the chamber floor.
- **The Vargr.**
  - Its gait keeps its feet on the floor; it turns to the monolith; its charge stops short of the wall.
  - The leap rises over clear floor and comes down on its announced landing disc.
  - Its corpse settles beside the monolith with nothing in it.
- **The Draugr.**
  - The entrance rises from the blade-supported crouch.
  - The Spear crystal forms in the raised hand.
  - Burial cracks, Edict runes and Requiem rings sit inside their regions under the roof.
  - The final-stage transition and the final stage are captured. The transition frame does not
    establish, on its own, that the mask's fall reads.
- **13 and 25 blocks.**
  - At 13 blocks both silhouettes, the aggro marker and the encounter reading are distinguishable, and
    a telegraph's dashed outline is visible.
  - At 25 blocks both bosses are small, the outline is faint and the reading remains legible.
  - No distant anatomical readability is claimed.

## Remaining limits

- **The Draugr's blade passes into terrain** when he strikes within reach of a wall or monolith:
  24–96 vertices in these frames. It is not corrected.
  - The server lets him stand there and announces the blow along its locked aim.
  - Retracting the blade at terrain would present a blow the server did not announce.
  - A terrain-aware weapon pose would be a new presentation system, not a correction.
  - Corrected since by #1103, which gives a planted blow a terrain-clear wrist variant along the
    same aim; see [the blade-terrain review](blade-terrain-1103.md).
- **The strike stroke is drawn under terrain.** Part 3a's strike stroke follows the announced region
  across the floor, so beside a monolith it runs under the monolith's base, as the region does.
- **The chamber's lighting, rotation and runtime gate** are not reproduced, as described above.
- **Reduced visual effects** stays blocked on #1093; no stand-in was built.

## Part 3a limits, revisited

- **Dungeon chamber:** captured. The shipped drawing is rendered by the production chunk renderer,
  within the limits above.
- **Reference box:** no longer used, so no stroke is occluded by one. Terrain now occludes what it
  occludes in play.

## Validation

- Client gates passed: `cargo fmt --all --check`, all-target Clippy with warnings denied, locked build
  and locked tests (2,420 passed, 22 ignored), including the new corpse-fall test.
- The arena capture passed separately.
- The automation shell suite and the DeepSeek Python tests passed.
- There are no new dependencies and no schema, server or audio changes.
