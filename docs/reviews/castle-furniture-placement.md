# Castle furnishing catalogue — issue #1203

This part activates the authoritative furnishing catalogue after the client renderer
in #1213. Protocol 45 rejects earlier clients that could decode the descriptors
without drawing their solid furniture. The voxel shape, footprint, stair flights,
NPC anchors and voxel worldgen version remain unchanged.

## Placed inventory

The capital contains 117 furnishing roots, with stable explicit slot identities.
Slots 79–82 and 95–98 remain unused after route and floor-support validation removed
provisional curtain benches and tower rugs. Slots 161–256 remain available for the
lighting follow-up; the combined catalogue remains bounded to 256 roots.

| Space | Standing floor | Furnishings |
| --- | --- | --- |
| East banquet hall | 0 | Two long tables, eight baked place settings and eight chairs per table; rugs and central runner |
| West kitchen/pantry | 0 | Preparation counters, food shelving, barrels |
| West library/study | 7 | Four bookcases, two writing desks and chairs |
| West council room | 14 | Map table, four chairs, bookcase, bench, bordered rug |
| West upper gallery | 21 | Benches, writing desk and chair |
| East audience hall | 7 (throne at 8) | Carved throne on the existing dais, audience seats, approach runner |
| East guard room | 14 | Benches, four equipment racks, counter, barrel |
| East upper gallery | 21 | Benches, bookcase, desk and chair |
| East overlook | 28 | Desk, chair, bench, bookcase, barrel |
| NW/SW/NE/SE spire interiors | 35/29/41/35 | Scholar's desk and chair in each room |
| Nine main floors | 0–28 | Wall-attached banners, shields and trophy displays |

The placement rows are the inventory source in `schematic_keep_furniture.go`.
Bookcase variant 3 draws pantry jars, sacks and scored loaves inside the unchanged
shelving collision envelope. Other variants continue to share library meshes.
Wall details now end at the supporting wall plane rather than floating at the
middle of the root cell; their streaming envelopes include their actual offset.

## Authority and route evidence

The existing world catalogue, stable IDs, complete-set snapshots, chunk relevance,
collision index and reset behavior are reused. No interactive structure, owner,
inventory, loot, persistence file or session entity ID is introduced.

The production movement walkthroughs now use the actual furnishing catalogue,
transformed with the same offset and four rotations as the architecture. They
exercise both stair/curtain lanes, all main floors, bridge, dais and four spire
lookouts, with normal gravity and collision. The provisional curtain benches
blocked the second lane and were removed; the tower rugs crossed unsupported
stair edges and were removed. No circulation route was moved to fit a prop.

Additional checks reject furniture/architecture penetration, intersections between
physical furniture members and unsupported feet. Each woven rectangle is checked
against every covered floor-cell patch, including full patch coverage by a support
shape, rather than merely testing a few points. Wall decorations must touch their
support and not penetrate architecture. Actual placed visual envelopes remain in
the permanent capital ward over five seeds and all four keep rotations.

The earlier solid-provider tests continue to cover saved arrival/welcome parity,
respawn obstruction, complete-set snapshot lifetime, NPC/trade visibility, and
fast projectiles against thin chair legs while allowing actual gaps.

## Remaining acceptance

This is the placement and compatibility part, not the visual acceptance report.
The follow-up uses the production castle capture harness to feed these actual
server-authored descriptors through the client snapshot consumer, inspect the
banquet/throne/study/tower views and record matched rendering costs. #1204 owns
window openings, candle fixtures and combined day/night acceptance. No light
fixture is activated by this catalogue.
