# Draugr king choreography, part 1 of 2 (#1034)

The existing king now has a held overhead Sentence, opposite horizontal first and
second Tolls, a forward third thrust, and separate Burial, Edict, Spear and Requiem
hand poses. The current server move, phase, combo step and remaining ticks select
those poses directly. New visibility in the third toll shows that thrust immediately;
replacement, expiration and cancellation discard the old move pose.

The cosmetic entrance is limited to the first idle appearance. There is no entrance
message in the current contract, so this is a blade-supported rise, without claiming
an authoritative throne sequence. Movement or an encounter announcement cancels it.
Snapshot displacement drives alternating foot support and turns. A death folds the
rig and settles its actual vertices on the local floor, while the snapshot continues
to own the root position and yaw.

This part preserves the geometry while redistributing fifteen segments: 1,464 triangles, 2,928 vertices and one
material. Three identically moving cloak strips become one mesh, freeing two ankle joints.
Arms and legs use rigid transforms; the blade stays attached to the right
palm. There are no added particles, camera motion or optional effects. Existing
hazard geometry, cast labels and countdowns remain the essential baseline. The
settings and UI currently have no reduced-effects switch. Part 2 owns mask/crown
articulation, the existing recessed core's transition and bounded spell/rune shapes.

## Reproduction

From `<repo-root>/client`:

```sh
cargo test --locked player::mobs::king -- --nocapture
cargo test --locked player::mobs::king::capture::capture_king_choreography -- --ignored --nocapture
```

The second command requires a GPU adapter and writes `king-1034-*.png` and
`king-1034-blade-reach.csv` into the platform temporary directory. It runs the
production mesh builder, snapshot consumer, animator, encounter reconciliation,
hazard cues and encounter UI. The fixture uses 20 Hz catalogue durations, the
three server Tolls regions and the first announced ritual pulse. Its neutral floor
and player-sized boxes are review geometry, not a dungeon collision simulation.

## Review record

The review runs offscreen at 1280 x 720, using the current default field of view,
on an AMD Radeon RX 5700 XT with RADV/Mesa 25.2.8 and Vulkan. This records the
adapter used for inspection; it is not a frame-rate guarantee.

The capture set includes preparation, release, pulse and recovery endpoints;
front/side/rear and 13-/25-block preparation views; entrance, walking/turning and
death samples; and late third-toll, replacement, turned-root and cancellation
states. The left hand is checked against the second grip in every two-handed pose,
not merely against the arm's own endpoint. The ankle split lets the entire sole
stay flat while the knee bends. The final corpse rests its torso on the floor.

Manual inspection completed on 2026-09-08. The physical preparations show a high
vertical blade, opposite cuts and a forward thrust; the cast preparations use a
planted blade, pointing hand, raised casting hand and chanting posture. The second
hand meets its grip at the overhead and thrust endpoints. Flat boots support the
crouched recovery, and the torso supports the settled corpse. No blade/crown or
blade/chest intersection was visible in the sampled extremes. At 25 blocks the
small armour details are not reliable signals, and the rear cloak masks much of
the hand detail. The silhouette, cast labels and floor regions remain the baseline
cues; part 2 must retain essential spell shapes and runes.

Recorded sheets: [physical poses](draugr-choreography-1034/physical.png),
[casts](draugr-choreography-1034/casts.png),
[blade views](draugr-choreography-1034/views-blade.png),
[cast views](draugr-choreography-1034/views-casts.png),
[13/25-block views](draugr-choreography-1034/distance.png),
[lifecycle and late visibility](draugr-choreography-1034/lifecycle.png),
[pulses](draugr-choreography-1034/pulses.png), and
[reach/replacement overlays](draugr-choreography-1034/reach.png).

Validation: workspace format, all-target Clippy with warnings denied, build and
all 2,383 tests passed (15 opt-in tests ignored); the GPU capture was run separately
and passed. All automation shell tests and DeepSeek Python tests passed. No new
runtime dependencies, protocol changes or server behavior were introduced.

## Physical reach handoff to #1037

The [release-tick CSV](draugr-choreography-1034/blade-reach.csv) records actual blade-mesh extrema across every 20 Hz release tick, with the
snapshot root at zero and its yaw at zero. Cone bearings are -0.45 and +0.45
radians. No weapon scaling, detached extension or speculative damage effect is
used to fill the remainder.

| Move | Peak visible reach | Server region | Remaining distance |
| --- | ---: | ---: | ---: |
| Sentence | 1.520 forward | 5.0 line | 3.480 |
| First toll | 1.567 radial | 3.8 cone | 2.233 |
| Second toll | 1.560 radial | 3.8 cone | 2.240 |
| Third toll | 1.740 forward | 3.8 line | 2.060 |

The line widths remain 1.1 for Sentence and 0.65 for the thrust; the cone
half-angle remains 0.95 radians. These numbers do **not** establish visually fair
contact. The blades now execute distinct strokes, but the server's far boundary
still lies well beyond their visible reach. The overlay captures deliberately keep
a player-sized marker near that boundary. #1037 must reconcile the assembled
combat distance and contact, including moving targets and dungeon walls. This
neutral-floor review does not claim a dungeon collision or party playtest.
