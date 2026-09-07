# First dungeon: visual and spatial study

Design study for #1021. These are concept references and prototype constraints, not
production meshes or an encounter implementation. The source design is the approved
first-dungeon design in #1021; the gameplay rules remain in [the GDD](../GDD.md).

## Reference sheets

- [Guardian turnaround](guardian-concept.png) and [physical moves](guardian-poses.png).
- [King turnaround](king-concept.png) and [sword/cast poses](king-poses.png).
- [Dimensioned envelope proxies](envelopes.svg), the scale reference for implementation.

Generated with the built-in image tool and inspected on 2026-09-07. Turnaround
player miniatures are explicitly non-dimensional. Pose sheets convey key poses;
numbered repetitions show time, not duplicate bosses or summoned weapons. The
pose index below resolves the meaning of every illustrated move.

## Scale and collision

The current registry already commits to these boxes. Keep them for this study; a
future change requires a separate authoritative contract decision.

| Body | Width and depth (blocks) | Height (blocks) |
| --- | --- | --- |
| Player | 0.6 | 1.8 |
| Vargr guardian | 1.6 | 1.8 |
| Draugr king | 1.0 | 2.8 |

Sources: `server/internal/game/constants.go`, `server/internal/game/species.go`,
and the mirrored `body()` in `client/src/player/mobs.rs`. The guardian is as tall
as the player at its shoulder tufts, not several players tall. The king is 14/9
of the player's height. **The generated concept sheets are not scale drawings**;
their small accompanying figures are illustrative and must not be used to size a rig.
Use the dimensioned envelope drawing and the tables here for scale.

Movement collision, hittable body and attack geometry are separate concepts. The
current body boxes above are the first two; a blade or extended paw is not an
implicit extension of either. A later attack must announce its own damage volume.
Rest-pose geometry, crown, tail and carried blade must fit their allocated envelope.
Attack poses may articulate beyond the rest box only within an explicitly reviewed
visual sweep; they never move the snapshot's root or decide a hit.

## Art direction

The guardian carries its weight forward: a compact, tucked belly behind enormous
shoulders, low mobile jaw, asymmetric fangs, torn ear and three broken collar chains.
Fur is overlapping dark angular clumps with sparse frost tips. The iron splinter
and ash scars break symmetry. Small amber eyes remain readable without covering the
beast in light. No spell effects, summoned wolves or chain physics.

The king has the opposing vertical silhouette: crown welded to a broken helmet,
one skeletal cheek and one masked cheek, layered oxidised armour, ceremonial cords,
waist seals, narrow cold chest fissure, two-handed funeral blade and rigid ice-cloak
strips. The free casting hand must remain visible. No shield. Crown, weapon and
cloak must remain distinguishable in front, side and rear views.

| Palette | Guardian | King |
| --- | --- | --- |
| Main | near-black fur `#24272b` | charcoal iron `#343b40` |
| Secondary | ash `#737779` | oxidised iron `#705044` |
| Light | frost `#c3d3d7` | bone `#b9b3a2` |
| Accent | amber eyes `#b9802e` | cold fissure `#81b7c1` |
| Shared | funeral iron `#454b4e` | funeral iron `#454b4e` |

Use linearised vertex colours within each moving segment. An ornament, tuft or
chain link is not an entity or material of its own. The same funeral iron connects
the collar, passage gate and royal armour.

## Pose index

Every row specifies a visible preparation, a release and an earned recovery. These
are authoring poses; #1019 uses only the existing snapshot `MobAction` vocabulary.
The full repertoire needs the subsequent encounter-state and animation issues.

| Move | Preparation | Release | Recovery / next signal |
| --- | --- | --- | --- |
| Morso e strappo | low head, raised lip, loaded shoulder | forward bite | exposed flank; second bite has a new preparation |
| Carica del collare | two scratches, neck taut, direction locked | straight dash | heavy stop; longer shoulder slump at a monolith |
| Balzo del predatore | crouch, gaze and fixed landing marker | leap to that point | front paws planted, head exposed |
| Artigli del carcerato | right or left paw raised, claws spread | sweep on that side | planted paw, exposed flank; other paw rises before a combo hit |
| Fauci spezzaossa | still stance, jaw fully open, neck loaded | narrow bite, no grab | jaw closed on empty air, long lowered-neck opening |
| Sentenza del re | blade high, held pause | narrow vertical slash | blade stuck in floor |
| Tre rintocchi | separate left, right, thrust preparations | corresponding cut, cut, thrust | marked recovery after the third; no rings or spells |
| Sepoltura | planted sword, successive concentric cracks | expanding rings with a safe band | king remains anchored |
| Editto delle tombe | free arm points at grave groups | numbered sectors erupt in shown order | arm lowers; ritual ends before next physical move |
| Lancia del sepolcro | free hand up, crystal forms, direction locked | straight non-homing ice lance | open hand, exposed chest |
| Requiem dei sepolti | planted sword, three notes, sectors shown | three announced sector pulses with safe gaps | long recovery; successful authoritative interrupt cancels pending pulses |

Entrance and transitions preserve control: guardian scratches, tugs collar and
turns its head; king rises leaning on his blade. No forced combat camera. Guardian
phase two breaks the remaining strap; king's final phase drops the mask and tilts
the crown, without changing the collision box. Death cancels cosmetic preparation;
a cosmetic flinch never cancels an authoritative attack.

## Arena prototype specification

These dimensions are outer shell dimensions, including one-block walls. Floor top
is Y=1. Keep the interior flat and dry so the measured walking speed applies.

| Surface | Outer footprint | Clear floor | Clear height |
| --- | --- | --- | --- |
| Guardian courtyard | 29 x 29 | 27 x 27 | 8 |
| King hall | 33 x 33 | 31 x 31 | 8 |
| Connecting gallery | 7 wide x 9 long | 5 wide | 5 |

The clear gallery admits either body with lateral clearance. Four one-block
monoliths may stand at courtyard offsets (+/-8,+/-8), leaving a broad central
cross and room between each pillar and wall. Keep arrival and exit slots out of
hazard samples. The king's throne is decorative and stays beyond the playable
floor or against its far wall. #1022 owns the actual drawing, anchors, rotations
and progression gate; this study must not replace the live chamber.

WalkSpeed is 4.3 blocks/s; at zero hunger it is 3.44. Use ordinary strafing with
no jump, invulnerability or new dodge. For initial major preparations of 0.9,
1.2 and 1.5 s, reserve 0.25 s for reaction/transport and one simulation tick.
The prototype must report both fed and starving movement at 20 and 60 Hz, and
include blocked paths as negative controls. The delay is a stated design allowance,
not a measured network guarantee. A late snapshot with less time needs later
network-state handling, not a promise from this study.

Provisional escape targets are 1.0 block laterally for a narrow strike, 1.5 for
an arc or charge corridor, and 2.0 for a landing zone or safe sector transition.
Measure the whole player's square footprint clear of a danger boundary, not just
its centre. Reject a proposed timing/width combination when the slow case cannot
clear it; do not silently increase movement speed. Guaranteed recovery starts at
1.2-2.0 s, with 2.5 s at a monolith, subject to combat playtesting.

## Initial authoring budgets

Caps below are initial authoring allocations, not measurements of final meshes.
The prototype report records the actual simple geometry separately. One merged
mesh per articulated segment and one shared lit material per boss are the baseline.

| Resource | Guardian cap | King cap |
| --- | --- | --- |
| Moving mesh segments | 19 | 17 |
| Triangles | 12,000 | 12,000 |
| Material handles | 2 | 2 |
| Concurrent cosmetic effect groups | 2 | 4 |

Guardian segments: pelvis, thorax, neck, head, jaw, eight limb sections, tail,
three bounded chain sections and two optional fur sections (19). King: pelvis,
thorax, head, eight limb sections, blade, three cloak strips and two optional
armour sections (17). Static detail is batched into these, never added on top as
unbounded children. A shared flash material is not a new segment.

Reference CPU for geometry/movement measurements: AMD Ryzen 7 3700X, x86_64 Linux.
Record the actual renderer and resolution if a GPU preview is run; CPU counts and
headless tests make no FPS claim. A final GPU performance budget remains a measured
follow-up, not a claim this concept study can establish.

## Review procedure

Inspect front/side/back, exact scale comparison, black silhouettes at 13 and 25
blocks, then each preparation/release/recovery. Verify planted feet, crown clearance,
blade clearance and readable casting hand. Record what was actually inspected;
concept inspection does not count as an in-engine animation or audio audition.
The spatial prototype tests must exercise the existing movement/collision code,
including a blocked route and insufficient preparation, rather than restating
speed times time as a successful arena test.
