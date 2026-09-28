# Chest client presentation and capture

References #1315. Implementation and the manual capture workflow ship before visual
acceptance. The follow-up evidence PR records actual images, run URL and source SHA.

The server sends Chest (64) or ChestOpen (65). The mesher draws a bronze-banded wooden
box, with a horizontal closed lid or an upright open lid and dark interior. All vertices
stay inside the authoritative solid cell. Neither state occludes neighbouring geometry.
The client never changes a chest block or remembers a local opening result. Amber runic
inlays follow the body corners, rim and lid through the existing unlit rune mesh; the
wooden shell and bronze fittings remain in the ordinary lit terrain pass.

The existing aimed-mechanism pass recognizes either block. The existing station prompt
shows `F: Chest` under the crosshair, substituting the configured Interact key. Corpse
loot retains priority, then an aimed chest, station, ordinary mechanism, player and resident.
Only the existing interaction sender emits `MechanismUseRequest`; no parallel sender is
installed. `ChestAlreadyOpened` becomes `Already opened`. A matching pending chest's
`MechanismLocked` becomes `Locked while the king lives`; lever/rune refusals keep their text.

## Loot presentation correlation

`LootState` names a container, without a request ID or cell. The client therefore keeps
one queued chest request at a time. A dropped or disconnected outbound request creates
no pending context. Snapshot-observed mob IDs remain corpse-only for this world, so a late
corpse response cannot bypass the existing snapshot, dismissal and revision guards.
Only a server reply to a pending chest associates a container with its aimed cell.
That association is presentation context, never proof of an opened block or loot rights.

A requested reopen may display an equal revision; unsolicited replies and older revisions
cannot reopen a dismissed chest. Death, changing input mode or a five-second timeout cancel
the pending presentation but retain one outstanding-reply tombstone. Only a new chest interaction
waits for that response or its matching anchored refusal to drain; the slot is not reused
for another chest while a late reply can still arrive. Server close events close the window.
While a chest response is outstanding, its prompt reads `Waiting for chest response`
instead of advertising an actionable key. Corpse, station, ordinary mechanism, player and
resident interactions remain available.

All container associations, observed mob IDs and pending/tombstone context reset with the
world or session, like the existing loot revision and dismissal maps.

## Manual remote evidence

After the workflow exists on `develop`, dispatch **Chest visual capture** on `develop`
with `source_sha` set to the full merged commit being reviewed. The workflow rejects a
commit outside develop ancestry. It runs only on manual dispatch, with read-only repository
permissions; it does not add a workload to ordinary CI or Integration.

The remote runner installs Mesa's software Vulkan driver, selects its ICD, exports both
sand-hall fixtures through the production server gate and runs the existing opt-in dungeon
capture harness twice. The only fixture difference is the sand chest cell's authoritative
Chest-to-ChestOpen update. Both images use the same standing camera, exactly five blocks
from the chest centre, production AcesFitted tonemapping, production cave grade and the
server's sconces. No light or exposure is added for the camera.

Download artifact `chest-visual-<source-sha>` before its seven-day expiry. It contains:

- `chest_closed.png` and `chest_open.png` at 1280×720.
- Per-image manifests with source SHA, camera, distance, adapter and lighting details.
- Server fixture/snapshot provenance and image checksums.

Inspect both actual PNGs at full size. Acceptance requires an identifiable closed chest
and a visibly raised lid/dark opening at the stated distance in the sand hall. Copy the
reviewed images and their provenance into the follow-up evidence PR and link the successful
workflow run. A green ordinary CI run does not establish this visual acceptance.

## First remote result and corrective scope

[Capture run 36450141868](https://github.com/FabioSM46/voxelheim-v2/actions/runs/36450141868)
succeeded at source `3964691d9597cd8cff3f50497fdab2088159e9b7`, but inspection of both
actual images **failed visual acceptance**: the closed/open lid outline barely differed
against the nearly black box. The manifests report five blocks, 1280×720, llvmpipe Vulkan,
seven sconces and three active point lights. Successful rendering did not mean readability.

The source explains the result. The sand chest is at drawing cell (17, 10, 35), while the
nearest sand-hall sconces are along x=1 and x=33, z=30. Their wall-light range is nine blocks,
so neither reaches the central chest. The production sand grade disables the sun and
reduces ambient brightness to one fifth. Merely counting active lights concealed that gap.
The fixture's closed/open block assertions passed; the camera and cave-light pipeline were
working as configured.

The corrective rendering adds thin amber rune channels around the body rim/corners and
on the lid, including a small maker's mark. They use the existing shared unlit rune material,
change position with the actual lid geometry, and stay inside the same solid voxel. The
wood remains shaded, the dark open recess remains visible by contrast, and no point light,
room illumination, camera/exposure change or capture-specific enhancement is introduced.
The capture workflow and camera are unchanged. A second remote run and direct inspection
of its PNGs are still required before visual acceptance can be recorded.
