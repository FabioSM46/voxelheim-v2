# Castle western tower interiors — issue #1202, fourth delivery

Internal two-wide switchbacks now connect the west gallery at Y21 to the
northwest lookout at Y35 and southwest lookout at Y29. Three-block flights
alternate sides of each 7 × 7 core, with two-deep landings and a final two-block
rise. Portals join the existing upper-gallery routes, while lower masonry and
the differentiated exterior capitals, tapers and spire tips remain intact.

The lookout stair openings have northern and eastern guards. The eastern guard
preserves the lower return flight's headroom; replacing that opening with a
floor would obstruct real-player ascent. Verified 3 × 2 furniture pockets lie
at NW X9–11/Z12–13 and SW X19–21/Z32–33, with complete two-wide southern routes.
South-facing glass slits sit above intact sills; barred-window content belongs
to the lighting follow-up.

## Movement and generation evidence

`TestCastleWesternSpireLookoutsAreReachedByWalking` drives production
`Player.step` from the gate through the main west stairs and each tower to its
lookout, then reverses the whole route. Both tower tread lanes pass in all four
rotations with normal gravity, no jump, no overlap, no fall damage and no
position correction. Independent tests inspect every two-deep landing, both
lookout furniture pockets, southern circulation and the opening guards. The
complete room-connectivity test remains enabled.

The former solid-centre assertion now inspects exterior shaft masonry, retaining
its independent capital, full-width spire, taper and tip checks. Worldgen changes
from 34 to 35. The envelope remains 63 × 63 × 68, and regenerating the existing
sampled golden chunks produces no binary changes.

Three samples of ten iterations compare the unchanged parent through a Go
source overlay with the actual new sources:

```text
go test ./internal/world -run '^$' -bench 'BenchmarkGenerateIn(ACapital|OpenCountry)$' -benchtime=10x -count=3
```

| Benchmark | Before ns/op, three runs | After ns/op, three runs |
| --- | --- | --- |
| GenerateInACapital | 6,451,426 / 7,980,514 / 5,813,057 | 6,665,750 / 6,284,985 / 5,748,600 |
| GenerateInOpenCountry | 6,540,522 / 9,266,809 / 9,362,763 | 9,468,966 / 6,381,092 / 9,540,684 |

Capital medians are 6.451 ms before and 6.285 ms after. Concurrent workloads make
these small samples noisy, including the unchanged open-country control. This
is a generation-cost smoke check, not an established speedup.

## Remaining work

This part references #1202 without closing it. The two eastern tower interiors
and final actual-renderer captures/walkthrough remain later work. The private
capture prototype is not part of this delivery, and no final visual acceptance
is claimed here.
