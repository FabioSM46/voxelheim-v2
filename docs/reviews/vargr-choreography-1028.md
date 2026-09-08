# Vargr choreography review — #1028, part 2 of 2

This extends the [accepted articulated rig](vargr-rig-1028.md) with the physical move
repertoire. Root translation and facing still come from snapshots. The server owns
move selection, combo position, phase windows, landing location, collision and damage.
This review records animation geometry and GPU inspection; the reach differences below
remain explicit inputs to #1037's final encounter acceptance.

## Rig and sampling

The existing collar cuboid is split into two rigid halves, preserving its exact rest
union and palette. The complete guardian now uses 19 segments, 648 triangles, 1,296
vertices and one shared material; no per-boss effects are added. The budget remains
19 segments / 12,000 triangles / two materials. Cues and UI retain their existing
independent budgets. The historical exterior-ray and colour comparison still checks
the accepted model rather than merely comparing new meshes to themselves.

Pelvis, thorax, neck, head and jaw compose a local matrix chain. The hip follows its
actual torso/pelvis transform; a two-bone solve joins each rigid upper leg and lower
leg at the knee. Supporting soles rock onto their lowest edge without changing their
world-space contact. Active paws lift and sweep within the same finite bones.
There is no mesh scaling or deformation. Neck/shoulder overlap from part 1 is retained.

Each phase samples its own announced tick count. The final sample is the last tick
inside that window; local render time never advances into a release or invents the
next blow. A one-tick interval samples its endpoint. Expired, upcoming, cancelled and
empty states contain no retained attack. A late second blow samples step 2 directly.
A replacement is read whole without counting previously observed instances.

The server stage ordinal opens the collar fastening. Idle stage two lowers the
posture and opens the mouth, including on first sight after the transition. During
an announced attack the stage change opens the fastening without replacing its pose
or shortening a recovery. The corpse retains the last announced fastening after the
inbox evicts the dead encounter. A new root starts from stage one.

## Repertoire

| Move | Preparation | Release | Recovery |
| --- | --- | --- | --- |
| Bite and tear 1/2 | Low head, open jaw, loaded shoulder | Forward closure | Low head and exposed first flank |
| Bite and tear 2/2 | Fresh opposite shoulder/neck load | Opposite tearing turn | Broader opposite flank opening |
| Prisoner claws, single | Right paw lifted, pale claw edge exposed | Forward/inward right sweep | Paw planted, flank open |
| Prisoner claws 1/2, 2/2 | Left then right paw, each with its own full preparation | The explicitly announced side sweeps | Planted support and open flank |
| Collar charge | Two separate complete paw scrapes, then taut neck | Low running pose over snapshot displacement | Heavy planted shoulder stop for the whole announced recovery |
| Predator leap | Deep crouch, raised look toward locked aim | Bounded child-mesh arc, tucked limbs, final-tick plant | Front support compression and exposed head |
| Bonebreaker jaws | Still stance, raised loaded head, widest gape | Sharp narrow jaw closure | Long low-neck opening |

The charge contract contains no impact-cause event. Its heavy stop also represents a
monolith impact, held for whatever recovery the server announces. No client collision
result or duration threshold classifies it as a monolith hit.

The leap server currently moves horizontally. The visual arc lifts only mesh children
by at most 0.62 blocks; the authoritative root does not rise because of animation.
Landing is sampled on the final release tick. The announced disc is an impact area,
not a claim that every damaging point is literal paw contact.

## Measured striking reach and handoff to #1037

Measurements use actual transformed vertices over the announced release windows,
including the extreme poses, in each blow's locked root-local frame. Fang vertices
measure bites/jaws, the active claw plate measures scratches, and all four claw plates
measure direct paw coverage at landing. Supporting/rear body surfaces are not claimed
to lie inside a forward bite cone. No accepted body part is stretched to fill a radius.

| Move | Announced radius | Maximum visible contact radius | Radial difference |
| --- | ---: | ---: | ---: |
| Bite 1/2 | 3.000 | 0.885 | 2.115 |
| Bite 2/2 | 3.000 | 0.893 | 2.107 |
| Single scratch | 3.400 | 0.978 | 2.422 |
| Left/right paired scratch | 3.400 | 0.978 | 2.422 |
| Leap impact area versus direct paw contact | 3.000 | 0.876 | 2.124 |
| Bonebreaker jaws | 3.600 | 0.834 | 2.766 |

Units are blocks. These visible striking vertices fit the announced cone/disc radius,
angle and height. Containment alone does **not** explain damage out to the announced
boundary: a bite or scratch can currently damage more than two blocks beyond the
visible fang or claw. #1037 must judge that unexplained reach and may tune the server's
provisional ranges consistently with the geometry. Leap acceptance must separately
judge whether its physical impact communicates the whole damaging area.

Charge radius is a different quantity: 9.900 blocks of **root travel**, with an announced
half-width of 1.400. The sampled body's visible half-width is 0.750, leaving 0.650 blocks
on either side. At the lane end the root remains inside the announced travel prefix,
but the accepted body's nose extends 0.842 blocks beyond it. This is measured body
overhang, not additional damaging travel. Whole-body containment in the lane is not
claimed, and the model is not shifted backwards or stretched to conceal the difference.

The reproducible tests write `guardian-1028-reach.csv` and `guardian-1028-charge.csv`
to the system temporary directory. The reference target in review images is 0.6 × 1.8
blocks; the boundary reference and production hazard outline expose the distance
between the body and the current announced range.

## GPU review record

The ignored `capture_guardian_choreography` harness runs the production snapshot
consumer, animator, timeline reconciliation, guardian pose pass, hazard cues and
encounter UI. It uses actual mesh assets and shared material handles, not a CPU drawing
of transforms. Frame output is 1280 × 720 at the configured default field of view.
Reference hardware: AMD Radeon RX 5700 XT, Vulkan, Mesa 25.2.8 (RADV NAVI10).

The matrix covers each listed blow at preparation/release/recovery start, midpoint and
last tick; front, both sides and distances of 13 and 25 blocks; the moving charge at
11 blocks/second and its planted stop; leap takeoff, airborne tuck and landing; late
second scratch, expired window, cancellation, replacement and death during preparation.
Fixture audit against the server catalogue: 60 Hz phase durations and combo recoveries,
locked aim in each blow's root-local frame, cone/lane centres at body half-height
(0.9), and the landing disc at its own half-height (1.5). The charge recording uses
54 consecutive release ticks at 11 blocks/second and the next recovery tick; leap
positions use 12 blocks/second clamped to the 6.5-block landing.
The review camera/lighting only makes surfaces inspectable; production materials remain
unchanged. The distant views preserve their real scale.

Run from `<worktree>/client`:

```sh
WGPU_BACKEND=vulkan cargo test --locked capture_guardian_choreography -- --ignored --nocapture
```

Full captures are named `guardian-1028-choreography-<move>-<phase>-<sample>.png`
in the system temporary directory. Selected unmodified GPU captures are linked below. This does not certify final dungeon lighting, latency, balance or
FPS; those assembled encounter checks belong to #1037.

## Recorded manual inspection

Reviewed on 2026-09-08: opposite raised paws, jaw gape/closure, the two scrapes,
charge support exchanges and planted stop, airborne tuck/landing, opposite bite
loads and the opened fastening with lowered stage-two posture are observable in
near views. No blocking joint separation or floor penetration was observed in the
reviewed extremes. Side/transition close-ups add review-only fill light and hide
reference pillars; ordinary front/distant captures retain their baseline lighting.

At 25 blocks the anatomical paw cue is tiny. Those frames do not establish full
distant anatomical readability; the unchanged hazard outlines and encounter labels
carry much of the readable information. The measured damage/contact differences
above and final dungeon lighting, latency and performance remain #1037 work.

| Inspection | Portable frames |
| --- | --- |
| Opposite bite loads and second opening | [First](vargr-moves-1028/bite1.png), [second](vargr-moves-1028/bite2.png), [recovery](vargr-moves-1028/bite-recovery.png) |
| Opposite paw preparations and sweep | [Left](vargr-moves-1028/claw-left.png), [right](vargr-moves-1028/claw-right.png), [sweep](vargr-moves-1028/claw-sweep.png) |
| Heavy jaws, lit side | [Open](vargr-moves-1028/jaws-open.png), [closed](vargr-moves-1028/jaws-closed.png) |
| Two charge scrapes | [First](vargr-moves-1028/scrape1.png), [second](vargr-moves-1028/scrape2.png) |
| Consecutive charge support and stop | [Step 18](vargr-moves-1028/run18.png), [step 36](vargr-moves-1028/run36.png), [stop](vargr-moves-1028/stop.png) |
| Leap and announced impact area | [Airborne](vargr-moves-1028/air.png), [landing](vargr-moves-1028/landing.png) |
| Fastening and posture | [Phase one](vargr-moves-1028/phase1.png), [phase two](vargr-moves-1028/phase2.png), [side](vargr-moves-1028/phase-side.png) |
| Gameplay distance | [Jaws at 13 blocks](vargr-moves-1028/13-blocks.png), [claw at 25 blocks](vargr-moves-1028/25-blocks.png) |
| Interrupted by death | [Opened-collar corpse](vargr-moves-1028/corpse.png) |

## Validation

Focused checks cover rigid joint continuity, support contact and floor containment
across every sampled extreme, accepted exterior/palette, distinct preparations and
claw sides, two scrapes, long recovery retention, actual reach measurements, 11-block/s
charge contacts and the production lifecycle through replacement, expiry, corpse and
despawn. A real production-system test verifies that snapshot root position/yaw stay
unchanged and that render time alone cannot advance an attack.

All four client gates passed: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo build --workspace --locked`, and `cargo test --workspace --locked`
(2,373 passed, 14 ignored). The full shell helper suite and Python DeepSeek suite also
passed. The ignored Vulkan capture is an additional manual-art check, not a substitute
for those gates.
