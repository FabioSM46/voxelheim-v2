# Boss strike reach presentation, part 3a (#1037)

[Part 1](dungeon-combat-1037.md) measured every planted blow landing well past the visible fang,
claw or blade, and [part 2](dungeon-corrections-1037.md) kept that reach wherever a player
standing there can strike the boss back:
- the Vargr's boundaries stay at 3.0, 3.4 and 3.6 blocks, against its 3.7 engagement reach;
- the Draugr's boundaries now sit 1.56–1.78 blocks past his blade.

This part presents that gap on the client. It changes no schema, no server code, no audio, and
no region, timing or outcome the server decides.

## What is drawn

`client/src/player/encounters/strikes.rs` adds one floor stroke per announced region of a planted
blow, only while that blow's release window is current on the newest snapshot tick:

| Blow | Stroke |
| --- | --- |
| Vargr bite and tear, bonebreaker jaws | Two jaw lines and the arc closing between them |
| Vargr prisoner claws, single and paired | Three furrows raked out across the cone |
| Draugr first and second toll | The blade's line swept across the cone at its far boundary, with a trailing arc |
| Draugr king's sentence, third toll | A cut along the strip, with a crossbar at its reach |

Everything comes from the announcement: its kind, window, release ticks and hazard volume.

**Reach.** The jaw lines, furrows and cuts start at the striking body's edge (half its width,
from the shared body registry). They grow with the share of release ticks that have run, and
reach the announced far boundary, less one 0.06-block stroke, on the last release tick. The toll
blade spans that same length throughout and crosses its cone as the ticks run.

**Nothing else draws a stroke:**
- a telegraph, a recovery, an upcoming or stale window, or an ended move;
- a charge, leap, spear or ritual;
- a kind announced by the wrong boss.

Replacement and cancellation release the effect's mesh, like the spell layer. There is one
material, and one effect has at most 768 vertices. Nothing moves the model, the camera, the
snapshot root or the regions.

**Reduced visual effects.** This layer carries no information the boundary cues and readings lack,
and it is the kind of optional decoration a reduced-effects setting would govern. The client has no
such setting: that criterion stays blocked on #1093, and no stand-in was built.

## Tests

`cargo test --locked player::encounters` runs four new tests:
- **Containment:** every vertex of every stroke, on every release tick of all six planted-blow
  regions, lies inside the announced volume. Growing strokes never withdraw, reach within 0.1 of
  the far boundary on the last tick, and do not reach it on the first.
- **The toll blade** crosses its cone monotonically, tick by tick.
- **No stroke** is drawn for any phase, window, ending or move kind listed above.
- **Lifecycle:** replacement and cancellation keep exactly one mesh per live effect, and none once
  it is gone.

The existing cue and spell tests are unchanged, and pass with the shared mesh builder.

## GPU capture

From `<repo-root>/client`:

```sh
WGPU_BACKEND=vulkan cargo test --locked \
    player::encounters::strikes::capture::capture_strike_reach -- --ignored --exact
```

**How it renders.**
- **Pipeline:** the production snapshot consumer, animator, encounter pose pass, regalia, boundary
  cues, spell and strike layers and encounter readings, offscreen at 1280 × 720 and the default field
  of view.
- **Adapter:** AMD Radeon RX 5700 XT (RADV NAVI10), Vulkan, Mesa 25.2.8. This names the adapter, not a frame-rate claim.
- **Fixtures:** 20 Hz releases and regions from the server catalogue after part 2. The floor is
  neutral and the 0.6 × 1.8 box is a player-sized reference just inside the far boundary; neither is
  the dungeon arena.

**What it captured.** 64 frames:
- eight blows — bite, single claw, left paired claw, jaws, sentence, first, second and third toll;
- at three samples of their release: first tick, midpoint and last tick;
- each sample twice from the same frame state, strike layer hidden and shown;
- plus the last tick from 13 and 25 blocks at a 1.7-block eye height.

Before each shot the harness asserts that a strike is being presented. It waits for each file,
not a frame count.

**Sheets.**
- [Vargr before and after](boss-strikes-1037/vargr-before-after.png) and
  [Draugr before and after](boss-strikes-1037/draugr-before-after.png): the last release tick
  of each blow, strike hidden and shown.
- [Release progression](boss-strikes-1037/progression.png): claw, jaws, sentence and first toll
  at the first tick, the midpoint and the last tick.
- [Distance](boss-strikes-1037/distance.png): claw, jaws, sentence and third toll from 13 and 25
  blocks.

## Inspection record

The agent inspected the four sheets on 2026-09-10. This records what the frames contain. A capture
is not a claim about what a player perceives in play.

- **Hidden.** Every blow shows the model and the red contact boundary. The floor between the body's
  edge and the far boundary is empty on the last release tick, which is the gap parts 1 and 2
  measured.
- **Shown.** The pale strokes run from the body's edge to the boundary:
  - the bite and jaws lines close on the far arc, which is narrow for the jaws;
  - the three claw furrows end on the arc;
  - the toll blade lies on the far edge of its cone, with the arc behind it;
  - the sentence and thrust cuts end at the strip's far edge.

  None was seen outside a boundary.
- **Progression.** The claw furrows and jaw lines are short on the first release tick, about half
  way at the midpoint and at the boundary on the last tick. The sentence cut grows the same way. The
  first toll's blade moves across its cone.
- **Occlusion.** In the sentence and third-toll views, the reference box covers part of the far end
  of the stroke. In the jaws view it stands on the narrow boundary.
- **Distance.** At 13 blocks the red boundary is faint and the pale strokes cannot be told apart. At
  25 blocks neither is reliable; the encounter reading, bar and marker above the boss remain
  legible. **The strike presentation is a near-range cue.** A player 13 or more blocks away is
  outside every planted blow's reach (at most 3.7), but no claim of distant readability is made.

## Remaining limits

- **Clipping and in-motion views** — feet and weapon clipping in extreme poses, gait and turns for
  both bosses at play distances — are part 3b. They are not claimed here.
- **The stroke is flat on the floor.** It does not bend the model or extend a weapon, so the fang,
  claw and blade themselves still stop where #1028 and #1034 measured.
- **Terrain.** The neutral floor ignores terrain. On uneven ground the stroke is placed at the
  announced region's floor height, as the boundary cues are.
- **Reduced visual effects** stays blocked on #1093.

## Validation

- Client gates passed: `cargo fmt --all --check`, all-target Clippy with warnings denied, locked
  build and locked tests (2,419 passed, 21 ignored).
- The capture passed separately.
- The automation shell suite and the DeepSeek Python tests passed.
- There are no new dependencies and no schema, server or audio changes.
