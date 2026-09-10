# Draugr king spells and final stage, part 2 of 2 (#1034)

Part 1 ([choreography review](draugr-choreography-1034.md)) gave the king his blade and
cast poses. This part presents what those casts announce and what the final stage does
to the body, from the same authoritative state:

- **Sepulchre Spear.** A crystal forms in the raised free hand for the announced
  telegraph and grows with its progress. On release a crystal crosses the locked lane at
  the server's rate — the whole lane over the release ticks, one equal step per tick —
  and stops before the first solid streamed voxel, as the server's flight does. It never
  follows a player, and nothing is drawn in recovery or once a window has expired.
- **Burial.** Crack lines light in order around the one annulus the server announces
  for the imminent pulse, and upright shards appear only on that pulse's contact tick.
- **Edict of the Graves.** Each announced sector carries a rune circle, rune staves lit
  in order, and a tally naming the pulse. Contact adds shards.
- **Requiem of the Buried.** Each sector shows one ring per intoned note and a ring
  closing on its centre. Contact adds spokes and shards.
- **Final stage.** The first stage-3 timeline for a body seen in an earlier stage drops
  the funeral mask to the floor and slips the crown over the bare brow; a light fills the
  chest fissure. A spell's release or pulse contact brightens that light in any stage.

The approved geometry is unchanged: 1,464 triangles and one material, redistributed into
17 segments by giving the mask and crown their own. The crown only moves down or inward.
The fallen mask lies inside the body box of the moment it fell and then belongs to the
floor rather than to the moving king. The core light sits inside the fissure. The root
position and yaw remain the snapshot's.

**Nothing here decides anything.** Stage comes only from `EncounterTimeline.phase`, never
from health. Shapes are sampled from `phase_started_tick`, `phase_ticks`, the pulse index
and the newest snapshot tick, never from a local clock, so a late, replaced or cancelled
move shows the current state and replays nothing. Every shape is generated inside its
announced volume; tests check each vertex against the volume on every tick of each phase.

## Late, replaced and withdrawn state

A body first seen already in stage 3 shows the tilted crown with no mask and replays no
fall. A living boss with no encounter is not in a fight, so a wipe's reset restores the
mask and crown. A corpse keeps what it showed after its encounter is evicted, and its core
goes dark. A cancelled, replaced or stale move removes its shapes and releases their
meshes. There are at most 512 effects, and each has at most 768 vertices.

## Reproduction

From `<repo-root>/client`:

```sh
cargo test --locked -- player::encounters::spells player::mobs::king
cargo test --locked player::encounters::spells::capture::capture_spells_and_regalia -- --ignored --exact
```

The second command requires a GPU adapter and writes `spells-1034-*.png` into the platform
temporary directory. It drives the production snapshot consumer, animator, encounter
reconciliation, boundary cues, spell layer, regalia and encounter readings, using the
catalogue durations at 20 Hz. Sector bearings in the fixture are review placements rather
than the server's bearing sequence. The neutral floor and player-sized box are review
geometry, not a dungeon.

## Review record

Offscreen at 1280 x 720 with the default field of view, on an AMD Radeon RX 5700 XT with
RADV/Mesa and Vulkan — the adapter used for inspection, not a frame-rate guarantee.

Manual inspection completed on 2026-09-10:

- The spear crystal reads in the hand at close range; from the front-left its grown tip
  crosses the helm brim, a cosmetic overlap that ends on release. The thrown crystal is legible
  along its lane, but at about fourteen blocks it is a small sliver: the lane boundary
  and the reading remain the essential cues.
- Burial's cracks advance outward pulse by pulse and stay inside each band. Edict runes
  and Requiem rings are distinct at a glance from above. Contact is identifiable by its
  raised shards as well as by the boundary's double line, so it does not rely on colour.
- The mask visibly falls mid-fight and rests beside the free-hand boot. Against the dark
  floor its iron is low in contrast, which is acceptable because it is not a cue. The bare
  bone face and slipped crown read from front, side and at 13 blocks. At 25 blocks the
  crown is marginal and the mask's absence is not reliable.
- The fissure light is brighter than the recessed ice of stages 1 and 2 and brightens
  further on pulse contact. Its effect on the overall silhouette is small.
- A late stage-3 body showed no mask and no fall. After death the core went dark; the mask
  stayed where it landed, which in this sample lies under the collapsed torso. A cancelled
  Edict drew nothing.

Sheets: [spear](draugr-spells-1034/spear.png), [burial](draugr-spells-1034/burial.png),
[edict and requiem](draugr-spells-1034/edict-requiem.png),
[regalia](draugr-spells-1034/regalia.png) and
[13/25 blocks](draugr-spells-1034/regalia-distance.png).

## Limitations

- **No reduced-effects setting exists.** This layer adds no particles, sound, light
  source or camera motion; its one brightness change is the core on an announced contact
  tick. It carries no information that the boundary cues and readings lack. However, a player cannot turn it off yet, so the acceptance criterion's
  reduced-effects wording is not claimed as met.
- The spear's wall stop reads the terrain this client has streamed. The contract carries
  no impact point, so a chunk that has not arrived draws the crystal on to the end of the
  lane.
- No audio, visual fairness of contact distances or dungeon collision is claimed.
  #1037 still owns reconciling contact distance.
