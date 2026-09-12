# Castle eastern towers and audience dais — issue #1202

The royal overlook at Y28 now reaches the northeast tower room at Y41 and the
southeast room at Y35 through internal two-wide stairs. Three-block flights
alternate sides, ending with a one-block rise. Northern parapets and raised
return-flight guard beams prevent a standing player from entering the stair
opening while preserving the lower flight's headroom.

The audience room gains a one-block-high dais inside the E7 western furnishing
pocket. Platform X38–41/Z20–23 stands at Y8; west-high stairs at X42 join the
clear X43–44 approach at Y7. Main circulation and all other furnishing pockets
retain their previous floor and clearance contracts.

Both tower routes and both dais approach lanes pass actual Player.step ascent
and descent in all four rotations. Additional collision tests push a standing
player toward each raised guard from the room and southern lane for 100 ticks;
the player neither falls nor overlaps the geometry. Independent tests inspect
every landing, furniture pocket, southern route and guard. Existing complete
room connectivity and the western/courtyard movement tests remain enabled.

Worldgen changes from 35 to 36. The 63 × 63 × 68 envelope is unchanged, and
regenerating existing sampled golden chunks produces no binary change.

## Generation cost

Three samples of ten iterations compare an overlay of the unchanged parent
sources with this geometry, using the same command:

```text
go test ./internal/world -run '^$' -bench 'BenchmarkGenerateIn(ACapital|OpenCountry)$' -benchtime=10x -count=3
```

| Benchmark | Before ns/op, three runs | After ns/op, three runs |
| --- | --- | --- |
| GenerateInACapital | 7,982,335 / 6,400,306 / 6,732,120 | 7,550,240 / 6,477,082 / 8,141,790 |
| GenerateInOpenCountry | 7,123,903 / 7,064,438 / 8,151,644 | 6,821,122 / 7,031,861 / 7,282,632 |

Capital medians are 6.732 ms before and 7.550 ms after. Concurrent compilation
makes these small samples noisy; this smoke check does not establish a
regression or speedup. The unchanged open-country control is shown explicitly.

## Remaining acceptance work

This independently complete geometry part references #1202 without closing it.
A separate capture/acceptance part supplies the reusable production-renderer
harness, exterior/interior frames and gate-to-every-lookout video evidence.
The complete assembled change is measured against the review cap before final
publication; capture code and prototype images are outside this geometry PR.
