# Castle grille geometry and review evidence

Issue #1204, geometry/compatibility part. IDs 58/59 append two oriented grille
shapes; protocol 43 rejects peers that would render these ids as full cubes.
This part does not install castle windows or enable lighting.

## Geometry and coverage

Each voxel contains two vertical bars, width 0.10 and depth 0.10, with a 0.40 gap.
The geometry, authoritative movement/projectile bounds and ray shapes use the
same occupied regions. Existing slabs/stairs retain their physical dimensions.

TerrainMaterial uses StandardMaterial's default back-face culling. Oppositely
oriented coincident faces therefore do not by themselves establish visible
z-fighting. Nevertheless, fully covered grille end caps are unnecessary geometry:
only top/bottom caps adjacent to a complete opaque cube are omitted. X/Z faces
remain inside the voxel. Sparse neighbours retain the caps conservatively.
Tests cover both grille orientations, both Y directions, internal and chunk-border
neighbours, missing/mismatched neighbours, slabs/stairs and adjacent grilles.
The neighbour cube's visible faces and ambient occlusion remain unchanged.

## Deliberate line-of-sight behavior

Authoritative sight now follows occupied slab/stair regions as well as grille
bars. It passes through an empty upper slab/stair half and stops at the real solid
half. This is intentional alignment with targeting and projectile geometry, not
an unchanged gameplay rule. Explicit DDA tests pin both outcomes.

The existing static-prop segment precheck remains before voxel traversal. The
voxel loop retains its direct solidity gate; only a proven-solid voxel calls
solidVoxelBlocksSight. Ordinary full cubes skip bounds/intersection work. The
collisionBlockReader path reuses CacheTerrain's revision-checked memo rather than
performing another fresh Peek. Missing terrain and synthetic solidity remain
full blockers.

An 8-block LOS microbenchmark uses resident deterministic voxel terrain with a
single obstacle halfway along the segment. Median ns/op of three 200 ms runs:

| Scene | Previous whole-voxel rule | Initial shape helper | Final shape helper |
| --- | ---: | ---: | ---: |
| Empty | 173.5 | 243.5 | 191.5 |
| Opaque wall | 125.0 | 213.6 | 136.2 |
| Grille bar | 110.9 | 234.9 | 218.0 |
| Grille gap | 120.6 | 378.0 | 290.2 |
| Empty slab half | 109.8 | 290.8 | 248.9 |
| Solid slab half | 124.0 | 188.8 | 181.7 |

The previous rule incorrectly stopped at the grille gap and empty slab half;
the new rule also traverses the remaining empty segment. This table measures
that changed behavior, not identical work or whole-game frame time. Other local
compilation can contribute timing noise. Reproduce the final column with
`go test ./internal/game -run '^$' -bench BenchmarkVoxelLineOfSight -benchtime=200ms -count=3`
from the server workspace. The legacy comparison replaces only the voxel-loop
predicate, retaining the same static-prop precheck and benchmark inputs.

## Twentieth-block units audit

`rg -n 'BlockBounds|collision_bounds|BOUNDS_SCALE|occupies_half' client/src --glob '*.rs'`
finds four implementation files:

- palette.rs defines 0..20 bounds and converts half-cell coordinates using ×10;
  face masks retain their original two-by-two grid.
- mesher.rs divides grille bounds by BOUNDS_SCALE. Existing slab/stair geometry
  uses occupies_half; its half-grid coordinates remain 0..2.
- player/target.rs divides all architectural bounds by BOUNDS_SCALE before exact
  ray-box tests.
- world/render.rs divides support/arrival bounds by BOUNDS_SCALE.

No other BlockBounds consumer remains. player/shapes.rs defines visual body mesh
primitives and consumes none of these bounds. The client has no local movement
collision prediction. Existing slab/stair geometry, targeting, face-mask and
arrival tests plus the full client suite validate these conversions.

Thin-obstacle collision is separately covered by the swept-axis probe tests and
480 authoritative arrow/orb bar/gap cases. The sweep preserves the previous
quarter-block substep cap and collision skin, including the shared static-prop
collision implementation integrated from #1203.
