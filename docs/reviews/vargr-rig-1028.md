# Vargr articulation review — #1028, part 1 of 2

Inspected on 2026-09-08 using the production snapshot consumer, mesh assets,
material swaps and animator. The render adapter was AMD Radeon RX 5700 XT,
RADV/Mesa 25.2.8, Vulkan, at 1280 × 720 with the current default vertical field
of view. These are actual GPU frames, not a CPU projection or a new concept.
No frame-rate guarantee follows from this inspection.

The resting exterior retains the accepted shoulder mass, tucked belly, jaw,
asymmetric fangs, torn ear, frost caps, tail and three collar chains. The same
geometry now has independent pelvis, thorax, neck, head, jaw, eight limb
sections, tail and three chain sections. Static detail stays batched within its
moving segment.

| Measured resource | Actual | Approved cap |
| --- | ---: | ---: |
| Moving mesh segments | 17 | 19 |
| Triangles | 636 | 12,000 |
| Vertices | 1,272 | — |
| Boss material handles at a time | 1 | 2 |
| Cosmetic effect groups | 0 | 2 |

The existing shared flash and lootable washes replace the material handle;
they do not add overlays or draw segments. Meshes remain shared between bosses.
The old three-mesh construction is retained only as a historical test fixture:
exterior ray and colour comparisons reject an accidental change of silhouette
or palette. The existing `export_boss_review_sheet` now reads the articulated
production meshes and transforms too.

## Recorded visual inspection

- [Rest](vargr-rig-1028/rest.png): the approved proportions and small amber eyes
  remain readable, with a 0.6 × 1.8-block player-sized reference beside the rig.
  Front, [side](vargr-rig-1028/side.png) and rear views were also captured and inspected.
  The side inspection found a breathing seam at the shoulder joint; internal
  overlap closes it while preserving the same rest exterior.
- [Walk](vargr-rig-1028/walk.png) and [turn](vargr-rig-1028/turn.png): consecutive
  frames show alternating diagonal support pairs, lifted returning paws and
  joined knees. Contacts are retained in world space while the authoritative
  root advances or turns. The body lowers to make room for bending rigid limbs;
  no mesh scales to reach its foot. The structural test also checks ordinary
  4.3-block/s movement, actual rendered support positions and knee continuity.
- [Fall](vargr-rig-1028/fall.png) and [corpse](vargr-rig-1028/corpse.png): the
  guardian rolls onto its shoulder and the upper/lower limbs fold separately.
  Segment bounds keep the fall above the floor, and secondary motion stops.
  The first GPU inspection exposed overlapping fur/bone faces on the soles;
  subdividing the existing paw volume removed that depth fighting without
  changing its exterior. These captures are from the corrected rig.
- [13 blocks](vargr-rig-1028/13-blocks.png) and
  [25 blocks](vargr-rig-1028/25-blocks.png): the broad front, pale fangs and heavy
  shoulder outline remain identifiable. Fine articulation is naturally less
  legible at 25 blocks; these resting views do not establish attack readability.

The root's position and yaw come from snapshots throughout the sequence.
Death is selected by the authoritative action, never by displayed health.
Late corpses start fallen; replacement and discontinuous position corrections
start with fresh support contacts. This is cosmetic floor contact relative to
where the server places the body, not a new terrain solver or movement rule.

## Reproduction and remaining part

From `client/`, run:

```sh
cargo test --locked capture_guardian_articulation -- --ignored --nocapture
```

This opt-in render test writes `guardian-1028-*.png` in the operating system's
temporary directory. It captures rest, front/side/rear, both gameplay distances,
twelve walking/turning frames and ten frames of the fall. It removes a previous
artifact before capture and checks for render-pipeline errors. Ordinary tests
need no GPU or display.

Part 2 authors each named preparation, blow and earned recovery from the received
encounter timeline, including combo steps, phase-two posture, charge stops and
leap landings. The current generic encounter preparation remains available on
the articulated rig; this first part does not claim the final attack repertoire,
audio audition or combat tuning acceptance.
