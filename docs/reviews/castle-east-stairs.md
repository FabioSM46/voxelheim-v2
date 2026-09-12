# Castle east-wing stairs — issue #1202, second delivery

Four two-wide shaped flights connect standing floors Y0/7/14/21/28. Northward
flights occupy X52–53, return flights X55–56, and landings Z27–29/Z37–38.
The reserved X49–50 approach stays clear. X51/X54 guard the runs and the
existing X57 exterior wall guards the return flight. The original courtyard
entrance at X46–47/Z39–41 remains in place.

The reserved X37–38 northern corridor removes only the inner half of the
courtyard wall at room height. Flat routes have two clear vertical cells for
the standing player; sloping flights are cleared separately. The full room
and furniture-pocket contracts remain in `docs/CASTLE_PLAN.md`.

## Movement and generation evidence

`TestCastleMainFloorsAreWalkedWithoutJumpingInEveryRotation` now covers both
wings: gate, original entrance, each flight, each upper room, and the return
route. It drives production `Player.step` at the server tick rate with normal
gravity and no jump or position correction. Both tread lanes pass in all four
rotations, including chunk-boundary crossings, with no overlap or fall damage.
The east route-reservation test independently checks every promised corridor
cell on all five floors. Existing complete room/anchor reachability remains
enabled.

Worldgen changes from 32 to 33. The envelope remains 63 × 63 × 68. Regeneration
of the existing sampled golden chunks produces no binary change; whole-castle
chunk determinism is still tested.

The following benchmarks use three repetitions of ten iterations. Before data
uses a Go overlay containing the unchanged parent commit's sources; after data
uses this delivery's actual sources. The executable command is the same,
except for that baseline overlay:

```text
go test ./internal/world -run '^$' -bench 'BenchmarkGenerateIn(ACapital|OpenCountry)$' -benchtime=10x -count=3
```

| Benchmark | Before ns/op, three runs | After ns/op, three runs |
| --- | --- | --- |
| GenerateInACapital | 6,693,228 / 6,167,904 / 6,456,655 | 6,508,581 / 5,840,670 / 8,410,634 |
| GenerateInOpenCountry | 6,627,040 / 9,325,499 / 6,461,218 | 8,662,148 / 6,491,950 / 11,715,482 |

Capital median is 6.457 ms before and 6.509 ms after. Concurrent workloads make
these small samples noisy, including the unchanged open-country control; this
is a generation-cost smoke check, not a statistically established regression
or speedup.

## Remaining work

This delivery does not close #1202. The courtyard stair and widened curtain,
corner lookout rooms, spire interiors, and real-renderer final captures remain
subsequent independently reviewed changes. No complete castle walkthrough or
visual acceptance is claimed yet.
