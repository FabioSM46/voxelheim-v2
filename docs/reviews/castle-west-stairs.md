# Castle west-wing stairs — issue #1202, first delivery

The west wing's three former cube-step flights are now two blocks wide and use
existing slate stair shapes. Both sides have guards. Upper rooms and the
carpenter's standing slot retain their access, and the furniture reservations
are pinned independently of the drawing's legend.

`TestCastleWestFloorsAreWalkedWithoutJumpingInEveryRotation` drives the actual
`Player.step` at the server tick rate. It walks from the gate, ascends each flight,
visits each west upper room, returns down all flights, and returns to the gate.
Both tread lanes are exercised in all four building rotations, across chunk
boundaries. There is no jump intent or position/vertical-velocity correction;
normal gravity, collision, and fall damage remain active. The route asserts no
body overlap, no upward displacement beyond the half-block walking step, and no
health loss.

World tests separately check all bottom/top stair orientations, actual placed
west-flight cells and unchanged cube/slab orientations. The existing exhaustive
room/anchor reachability and silhouette tests remain enabled. The worldgen
version moves from 31 to 32. Regenerating the existing golden chunks changes no
fixture: those sampled chunks do not include this edited part of the castle.
The existing whole-castle generation determinism test still compares every
intersecting chunk twice.

## Generation cost

Same command before and after, with three repetitions of ten iterations per benchmark:

```text
go test ./internal/world -run '^$' -bench 'BenchmarkGenerateIn(ACapital|OpenCountry)$' -benchtime=10x -count=3
```

| Benchmark | Before ns/op, three runs | After ns/op, three runs |
| --- | --- | --- |
| GenerateInACapital | 6,563,030 / 5,944,498 / 5,811,879 | 6,662,124 / 5,955,129 / 5,818,429 |
| GenerateInOpenCountry | 8,366,490 / 6,633,121 / 6,701,394 | 6,588,828 / 9,481,044 / 9,643,974 |

The capital median changes from 5.944 ms to 5.955 ms. The unrelated open-country
path shows substantial scheduling noise during concurrent development. These
samples are a cost smoke check, not a statistically established regression or
speedup. The footprint and generation algorithm remain unchanged;
placement additionally rotates the existing directional stair IDs.

## Remaining issue scope

East-wing stairs, courtyard stair and widened curtain/corner lookouts, all four
spire interiors, and the final real-renderer exterior/interior captures are
subsequent deliveries. This first change does not close #1202 and does not claim
that the whole castle can already be explored without jumping.
