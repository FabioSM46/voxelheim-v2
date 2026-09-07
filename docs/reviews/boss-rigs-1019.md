# Initial boss rig review — #1019

Inspected 2026-09-07: [actual mesh projection](boss-rigs-1019.png).
The rows are the guardian and king; columns are front, side, rear, windup,
recovery, corpse, 13-block and 25-block distance. The latter two use the current
45-degree vertical field of view and 1080 vertical pixels. This is a CPU projection
of the actual mesh vertices, not a captured GPU frame or a lighting benchmark.

The guardian's heavy shoulder tufts, forward jaw, torn ear, unequal fangs and collar
chains distinguish it from the common vargr. The king's crown, broad armoured torso,
mask, cold chest mark, blade and cloak distinguish it from the common draugr. The
king's shoulder and arm proportions are separately authored; this is not a uniform
scale of the common rig. The same snapshot presentation systems drive both.

Rest-pose vertices remain inside the boxes already committed by #1018. The king
raises the paired arms around its own shoulder height. The guardian rolls onto a
shoulder; the king folds forward on death. Health alone never selects either pose.
Snapshot replacement/despawn, death during a windup and return to chase remain
presentation transitions, with no encounter or reward state inferred locally.

The guardian has three mesh children, the king three, each using one shared white
material with authored vertex colours. Static detail is merged into its moving
part. #1031 owns the king's finer armour and independent articulation; the full
move repertoire remains separate from this initial MobAction presentation.

To reproduce, from `client/`, set `VOXELHEIM_BOSS_REVIEW_PATH` to a local SVG
filename and run `cargo test --locked export_boss_review_sheet -- --ignored`.
Open that SVG in a browser. The ordinary tests require no window or GPU.
No audio audition is claimed by this rig-only part.
