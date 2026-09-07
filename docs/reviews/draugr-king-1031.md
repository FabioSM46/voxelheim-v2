# Draugr king articulation review — #1031

Inspected 2026-09-07: [mesh review sheet](draugr-king-1031.png). This supersedes the
king row of the initial #1019 review; the guardian is shown as an unchanged visual
comparison. The concept and envelope study is #1021, delivered in #1045/#1046.

The king now has fifteen moving mesh segments: pelvis, thorax, head, paired upper
arms and forearms, paired thighs and shins, blade and three cloak strips. Shoulder
plates, crown spikes, mask, rivets, cords, seals and ice edges are merged into their
owning segment. Each creature reuses the startup mesh/material handles.

Measured from the actual meshes: **1,464 triangles, 2,928 vertices, 15 segments,
one material handle, zero effect groups**. This is within the initial allocations
of 12,000 triangles, 17 segments, two materials and four effect groups. The existing
1.0 x 1.0 x 2.8-block rest envelope is unchanged and every rest vertex is checked
against it. Reference CPU: AMD Ryzen 7 3700X, x86_64 Linux. No FPS result is claimed.

## Visual findings

- Front: crown, one skeletal cheek and one funeral mask remain distinct. Layered
  chest halves expose a recessed cold core rather than painting light over armour.
  Waist seals and the blade separate the king from the common draugr.
- Side: the cloak wraps over the upper back rather than floating behind it. The
  blade grip meets the right hand. Torso and belt interpenetrate without a gap.
- Rear: three unequal cloak strips have separate hinges and bounded cosmetic sway;
  they remain three meshes irrespective of the number of ice strips or ornaments.
- Sword preparation/recovery: shoulder and elbow both articulate; the blade follows
  the weapon forearm. Its geometry stays beyond the chest/crown on a separating
  axis throughout the sampled sword poses, including intermediate frames.
- Cast key pose: the free hand rises in front, while the blade remains attached to
  the other hand. This is an offline review pose only, not a locally inferred cast.
- Gameplay-size columns use the current 45-degree vertical FOV and 1080 vertical
  pixels at model-centre distances of 13 and 25 blocks. Crown, long silhouette,
  pauldrons and blade remain readable; fine rivets are detail at closer range.

The sheet projects actual mesh vertices on the CPU, with Chrome used to inspect
it. It is not an in-engine lighting capture. Extended sword/cast poses describe a
visual sweep beyond the standing box; they are not a damage volume or a promise
that a future attack may ignore walls. Authoritative attack geometry and placement
remain the later combat/arena work.

## Verified behaviour

Existing snapshot actions drive only cosmetic joint motion. The king's root stays
upright while alive so the articulated legs keep their local floor contact; torso
and arms carry the preparatory lean. Death stops secondary motion and retains the
existing authored forward collapse. The shared snapshot lifecycle test still checks
creation, action changes, health-without-death and complete child cleanup.

The tests check rest envelopes, segment/triangle caps, unique segments, relative
elbow and knee articulation, blade attachment, sampled crown/chest separation,
raised free hand, local floor clearance and stationary corpse secondary poses.
The full client gates and automation suite passed, with 2,275 client tests passing;
the manual sheet-export test was run explicitly in addition.

Reproduce the sheet from `client/`:

```sh
VOXELHEIM_BOSS_REVIEW_PATH=king-review.svg cargo test --locked export_boss_review_sheet -- --ignored --nocapture
```

No spell AI, hit detection, sound or full attack-animation repertoire is added.
