# Draugr king spells and final stage, part 2 of 2 (#1034)

Part 1 ([choreography](draugr-choreography-1034.md)) posed the king. This part presents
what his casts announce and what the final stage does to his body, from the same
authoritative state:

- **Sepulchre Spear.** A crystal grows in the raised hand over the telegraph. On release a
  crystal crosses the locked lane at the server's rate — the lane over the release ticks,
  one equal step per tick — and stops before the first solid streamed voxel, as the
  server's flight does. It never follows a player.
- **Burial.** Cracks light in order around the one annulus announced for the imminent pulse.
- **Edict of the Graves.** Each sector carries a rune circle, staves lit in order and a
  tally naming the pulse.
- **Requiem of the Buried.** Each sector shows a ring per intoned note and a ring closing
  on its centre.
- **Contact.** Only the server's release or pulse contact tick raises shards (and Requiem
  spokes), in addition to the boundary's double line, so contact does not rely on colour.
- **Final stage.** The first stage-3 timeline for a body seen earlier drops the funeral
  mask to the floor, slips the crown over the bare brow and lights the chest fissure. A
  spell's contact brightens that light in any stage.

Stage comes only from `EncounterTimeline.phase`, never from health; shapes come from the
announced ticks, pulse and newest snapshot tick, never from a local clock. Tests check every
vertex against its announced volume on every tick of each phase. A late, replaced, expired
or cancelled move draws only what is current and releases removed meshes, at most 512
effects of at most 768 vertices. A body first seen in stage 3 has no mask and replays no
fall; only a new encounter id or an earlier stage restores the regalia, never a missing
timeline; a corpse keeps what it showed and its core goes dark.

The approved geometry is unchanged: 1,464 triangles and one material, now in 17 segments
because the mask and crown have their own. The crown only moves down or inward, the fallen
mask lands inside the body box of that moment and stays on the floor, the core light sits
in the fissure, and the snapshot still owns root position and yaw.

## Reproduction

From `<repo-root>/client`:

```sh
cargo test --locked -- player::encounters::spells player::mobs::king
cargo test --locked player::encounters::spells::capture::capture_spells_and_regalia -- --ignored --exact
```

The second needs a GPU and writes `spells-1034-*.png` to the temporary directory through the
production consumers at 20 Hz catalogue durations. Its sector bearings, floor and
player-sized box are review placements, not the server's sequence or a dungeon.

## Review record

Offscreen, 1280 x 720, default field of view, AMD Radeon RX 5700 XT with RADV and Vulkan: the
inspection adapter, not a frame-rate guarantee. Manual inspection on 2026-09-10:

- The hand crystal reads at close range; from the front-left its tip crosses the helm brim
  until release. The thrown crystal is legible along its lane but a small sliver at about
  fourteen blocks, where the lane boundary and reading remain the essential cues.
- Burial advances pulse by pulse inside each band; Edict runes and Requiem rings are
  distinct at a glance; contact shards are clear; a cancelled Edict drew nothing.
- The mask falls visibly and rests beside the free-hand boot, low in contrast on a dark
  floor. The bare face and slipped crown read from front, side and 13 blocks; at 25 blocks
  the crown is marginal and the missing mask is not reliable.
- The fissure light outshines the earlier recessed ice and brightens on contact.
- A late stage-3 body had no mask and no fall. After death the core was dark and the mask
  stayed where it landed, in this sample under the collapsed torso.

Sheets: [spear](draugr-spells-1034/spear.png), [burial](draugr-spells-1034/burial.png),
[edict and requiem](draugr-spells-1034/edict-requiem.png),
[regalia](draugr-spells-1034/regalia.png), [13/25 blocks](draugr-spells-1034/regalia-distance.png).

## Limitations

- **Reduced effects arrived with #1093**, after this record: see
  [reduced-effects-1093](reduced-effects-1093.md). This layer adds no particles, sound, light
  source or camera motion; its one brightness change is the core on an announced contact
  tick, and it carries nothing the boundary cues and readings lack, which is why that
  setting can withhold it whole.
- The wall stop reads streamed terrain; with no impact point in the contract, a missing
  chunk lets the crystal continue to the end of the lane.
- No audio, contact-distance fairness or dungeon collision is claimed; #1037 owns distance.
