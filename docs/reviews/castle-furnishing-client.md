# Castle furnishing client foundation

Issue #1203, part 3 of 4. This part renders the static descriptors already decoded
by the client and enforces presentation occlusion using the authoritative solid
catalogue. The world catalogue remains empty: authored room placement and the
complete furnished-castle acceptance belong to the final part after #1202.

## Models and ownership

Sixteen furnishing families use code-built meshes: banquet table, chair, bench,
throne, bookcase, desk, counter, barrel, equipment rack, council table, rug,
runner, banner, shield, trophy and feast setting. The banquet model includes
eight repeated place settings, shared food platters and serving vessels; these
are baked into mesh parts, not extra network descriptors or entity roots.

Each family has at most five merged material parts. Four variants share the same
mesh handles, with eleven shared materials across the catalogue. Timber, iron,
textiles, brass and colored details retain a coherent palette. The 39 major
physical members are checked against the server's committed bounds fixture.
Bookcases intentionally use a filled shelving collision envelope.

`StaticPropRoot(StaticPropState)` owns every furnishing child. Fixture kinds have
empty roots here; #1204 attaches candelabra, flame and light children to these
same roots. `StaticPropsSet::Sync` includes an explicit deferred-command barrier,
so lighting ordered after it can observe newly created fixture roots that frame.

## Complete-set lifetime

Only the newest accepted snapshot supplies poses. Unchanged descriptors retain
their entity identities and asset handles; changed poses replace their roots.
Omitted roots recursively despawn, including children attached by lighting.
World/session reset clears both roots and ray bounds before another world's
snapshot can reuse the same prop identity. Removing the session also clears them.
Mesh and material assets are built once and reused across reconnects.

The wire decoder bounds the set at 256 roots. Each root has at most five furnishing
mesh children and six solid boxes. Frustum visibility remains Bevy's; cosmetic
non-fixture details additionally hide beyond 64 blocks and reappear nearby.
Major solid furniture stays visible throughout its snapshot relevance, avoiding
an invisible collision obstacle. Fixtures retain their own lighting owner's
visibility budget. Inventory/menu hiding follows existing structure presentation.

## Targeting

The exact transformed member boxes stop a voxel or station selection at the
nearest furniture hit; props gain no target type or request. Healing hints also
stop at furniture before a player body. A ray through a real gap remains clear.
The voxel ray still tests the aimed surface, so a visible corner is not rejected
because a separate ray to the voxel centre would be hidden.

Resident interaction remains distance based. Eye-to-eye furniture visibility
filters candidates before nearest-distance selection, matching the server's
rule and preserving interaction above low tables. Its input system explicitly
runs after `StaticPropsSet::Sync`; tests exercise a snapshot and interaction
press in the same operational update. Existing no-prop voxel-wall behavior is preserved.
This introduces no local movement prediction or gameplay authority.

## Verification scope

Focused tests exercise shared geometry and asset bounds, complete snapshot
replacement, stale snapshot rejection, recursive fixture-child teardown,
world/session exit, reused identities, full 256-root capacity, distance culling,
all four orientations, table gaps, visible voxel corners, real block targeting
and same-frame resident intent filtering.

The supplied oblique model scene was inspected with the production mesh/material
code. That illustrative scene is not a player-height castle capture or evidence
that final placements fit. Final room inventory, route/ward/spawn checks, banquet/throne/
study/tower captures, and before/after entity, draw-call and frame-time samples
on the same castle route remain required in part 4. Wall-mounted banners, shields
and trophies also require a rearward root-local mounting offset with matching
conservative world visual bounds; final placement must verify flush wall planes
in all four rotations. Their current centre-plane model origins are inactive. Active solid publication
must also reject earlier clients that decoded descriptors without this renderer,
using the then-current protocol compatibility boundary.

Local gates passed: Rust formatting, full Clippy, normal build, and the complete
client suite (2,648 passed, 24 ignored, no failures), followed by all automation
helper scripts and Python review tests. Publication privacy scans also passed.
