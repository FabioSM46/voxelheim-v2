# Castle courtyard and curtain — issue #1202, third delivery

The courtyard now reaches the wall walk through thirteen shaped northward
stairs, two cells wide at X4–5/Z43–31. Side guards follow the rise, and the
Y13 landing opens onto the west curtain at X1–5/Z29–30. Moving the stair east
of the widened western deck keeps the deck out of its sloping headroom.

The Y13 curtain forms a complete two-wide circuit. Four sheltered 7 × 7 corner
lookouts join it; their floor, standing clearance and inner parapets are checked
independently of route search. The existing exterior masonry remains intact.
Main-wing furniture pockets, anchors and the 63 × 63 × 68 envelope are unchanged.

## Movement and generation evidence

`TestCastleCourtStairAndCurtainCircuitNeedNoJump` walks the actual production
`Player.step` from the gate to the stair, around all four corner lookouts and
back down to the gate. Both tread lanes and both curtain lanes pass in all four
rotations. Gravity accumulates normally; the test checks overlap, step height
and fall damage without jumping or correcting position. Existing main-wing
movement, complete room reachability and furniture reservation tests still pass.

Worldgen changes from 33 to 34. Regenerating the existing sampled golden chunks
produces no binary changes; complete castle chunk determinism remains tested.

The benchmark command below runs three samples of ten iterations. Before uses
an overlay of the unchanged parent sources, after uses this delivery's sources.

```text
go test ./internal/world -run '^$' -bench 'BenchmarkGenerateIn(ACapital|OpenCountry)$' -benchtime=10x -count=3
```

| Benchmark | Before ns/op, three runs | After ns/op, three runs |
| --- | --- | --- |
| GenerateInACapital | 6,404,228 / 8,455,542 / 8,321,522 | 6,341,028 / 5,606,431 / 5,710,066 |
| GenerateInOpenCountry | 7,088,473 / 6,362,380 / 9,377,073 | 7,997,534 / 6,468,501 / 9,223,781 |

Capital medians are 8.322 ms before and 5.710 ms after. Concurrent workloads
make these small samples noisy, including the unchanged open-country control;
this is a generation-cost smoke check, not an established speedup.

## Remaining work

This independently complete circulation change references #1202 without closing
it. Interior staircases and rooms in the four spired towers, followed by actual
production-renderer captures and the complete lookout walkthrough, remain later
parts. This report does not claim final visual acceptance.
