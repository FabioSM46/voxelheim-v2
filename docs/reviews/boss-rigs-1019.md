# Assembled boss rig review — #1019 and #1031

Re-inspected after the assembled review on 2026-09-07: [actual mesh projection](boss-rigs-1019.png).
The rows are the guardian and king; columns are front, side, rear, windup,
recovery, corpse, 13-block and 25-block distance, plus the king cast key pose. The latter two use the current
45-degree vertical field of view and 1080 vertical pixels. This is a CPU projection
of the actual mesh vertices, not a captured GPU frame or a lighting benchmark.

The guardian's heavy shoulder tufts, forward jaw, torn ear, unequal fangs and collar
chains distinguish it from the common vargr. The king's crown, broad armoured torso,
mask, cold chest mark, blade and cloak distinguish it from the common draugr. The
king's shoulder and arm proportions are separately authored; this is not a uniform
scale of the common rig. The same snapshot presentation systems drive both.

Rest-pose vertices remain inside the boxes already committed by #1018. The king
articulates its upper/lower arms around its own shoulders and elbows. The guardian rolls onto a
shoulder; the king folds forward on death. Health alone never selects either pose.
Snapshot replacement/despawn, death during a windup and return to chase remain
presentation transitions, with no encounter or reward state inferred locally.

The guardian has three mesh children, the articulated king fifteen, each using one
shared white material with authored vertex colours. Static detail is merged into
its moving part. The king details and limits are recorded in
[draugr-king-1031.md](draugr-king-1031.md). The full move repertoire remains separate
from this MobAction presentation.

The assembled review found buried frost caps and fangs on the guardian. The fur
now ends below each frost cap, and the fangs extend beyond the jaw front. The
corrected projection shows both details; outward sightline tests reject covered
faces, while the existing rest-envelope test still passes. Geometry counts and
material counts are unchanged.

To reproduce, from `client/`, set `VOXELHEIM_BOSS_REVIEW_PATH` to a local SVG
filename and run `cargo test --locked export_boss_review_sheet -- --ignored`.
Open that SVG in a browser. The ordinary tests require no window or GPU.
No audio audition is claimed by this rig-only part.
