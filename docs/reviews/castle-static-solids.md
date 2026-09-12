# Castle static solids foundation

Part 2 of #1203 implements authoritative static furniture and its snapshot producer.
The authored placement list remains empty until the renderer and final room acceptance
land. This part does not activate invisible furniture or claim visual acceptance.

## Contract and lifetime

The one capital has at most 256 roots, including future lighting fixtures. Explicit
slots 1–256 occupy the low nine identity bits; a building/seed prefix occupies the
remaining bits. Reordering a placement list does not rename a root. World placement
rotates both the local cell and its facing through all four keep orientations.

Ten major furniture kinds have 39 exact local boxes; dressing and fixtures have no
collision. The shared `server/internal/world/testdata/static_prop_bounds.tsv` fixture
pins the model contract without adding a parser dependency. All four visual variants
use the same physical shape. North means -Z, with a horizontal cell-centre origin and
an exact floor-plane Y. Table, desk and counter tops are at local Y=1.

Simulation construction computes immutable bounds and a chunk index once. Each root
covers at most eight chunks. Local queries use those buckets; a long ray traverses at
most 256 roots instead of a large empty chunk volume. Ordinary queries neither generate
terrain nor allocate. Physical collision never waits for chunk materialisation or a
client snapshot. Materialisation changes only publication eligibility; complete
per-recipient view sets remove departed roots. Finite instances receive no capital
catalogue. No interactive structure IDs, inventory, ownership or persistence records
are introduced.

## Collision and action evidence

The existing voxel `Terrain.Solid` answer remains unchanged. A dedicated exact-box
query augments body overlap and obstruction rays. Single-axis movement probes the
union of the starting and destination boxes, including during contact bisection.
This is the exact swept volume for that translation and catches thin furniture legs
and voxel bars between endpoints without reducing the global substep size. The same
six-line swept-probe change is coordinated with #1204's grille implementation.

Tests cover both directions and 50 launch phases of a small projectile body: the
first chair leg stops it while the actual gap remains open. Tests also exercise
ordinary player movement, no-prop behavior and sight through a real table-leg gap.

NPC interaction and continued trade use the same eye-to-eye prop-only visibility
rule. A purchase over a banquet table succeeds; moving behind a bookcase closes the
stall and prevents reopening. Voxel-wall, distance and trade rules remain unchanged.

The capital ward already denies mining acquisition, ordinary block placement and
structure footprints. Its ownership cannot change during a simulation, so an
admitted mining target cannot later become a newly protected castle target. Tests
check the whole keep plus four blocks of overhang over five seeds and four rotations.
Final activation must additionally verify every actual prop-covered column. No
redundant mining or placement obstruction policy is added.

Before Welcome, a saved position occupied by new static furniture is normalized to
the existing world spawn. Join uses the same authority; the saved record is copied,
not mutated. Respawn candidates also reject static furniture. Focused tests pin the
Welcome/Join position, the first snapshot position, safe fallback, and unchanged
unobstructed/instance behavior. This is a prop-only correction, not a new spawn search.

## Focused performance measurements

These are synthetic Go microbenchmarks, not frame-time or full simulation results.
A local three-run ordinary movement comparison measured a median 262 ns/op before
swept probes and 296 ns/op afterwards: about +34 ns (+13%), with zero allocations in
both cases. Concurrent compilation can add noise, so this is an indicative local
comparison rather than a portable performance threshold.

Separate indexed query measurements were approximately 6.8 ns outside the capital,
83 ns for a furnished-room body probe and 125 ns for a table-gap ray; each reported
0 B/op and 0 allocs/op. An allocation regression test covers these three query shapes.

## Remaining delivery

The client lifecycle and furniture models are the next independently complete part.
Final placement will furnish the banquet hall, throne room, library, kitchen/service
rooms, council room, guard spaces, galleries and tower lookouts, while preserving the
architecture's reserved lanes. Lighting uses the same roots and lifetime.

At first active publication, bump the then-current protocol unless the last protocol
bump demonstrably occurred only after the rendering consumer was present. The earlier
descriptor version alone does not exclude a client that knows the descriptor but
cannot draw the newly solid furniture.
