# First dungeon combat corrections, part 2 (#1037)

[Part 1](dungeon-combat-1037.md) measured the assembled fights. This part corrects two of its
findings on the server and measures the result with the same harness: ritual pulses that did
not survive latency, and blows whose reach no engagement explains. Fight length is not tuned
here; balance is #1099. Every kill-time change below is a side effect of these corrections.

## What changed

| Value | Before | After |
| --- | ---: | ---: |
| Burial pulse shown for | 700 ms | 1.0 s |
| Edict of the graves pulse shown for | 800 ms | 1.3 s |
| Requiem of the buried pulse shown for | 900 ms | 1.3 s |
| King's sentence strip length / selection band | 5.0 / 4.0 | 3.3 / 2.8 |
| Three tolls cones and thrust / selection band | 3.8 / 3.4 | 3.3 / 2.8 |

In addition, a move that plants its creature is chosen, or its combination continued, only when
the region it would announce reaches its target at that moment (`mob.announcedRegionReaches`).
Travelling, thrown and ritual moves are unaffected. No Vargr value changed, no schema or client
code changed, and no health, damage or scaling value changed.

## Reproduction

From `<repo-root>/server`:

```sh
go test ./internal/game -run 'TestFirstDungeon|TestPlantedBlows|TestAPlantedMove' -v
VOXELHEIM_PLAYTEST=1 VOXELHEIM_PLAYTEST_DIR=<review-output> \
    go test ./internal/game -run TestFirstDungeonPlaytest -v
```

The before set is the part 1 matrix regenerated on `develop` at 10c6273; it was byte-identical
to the set #1098 was merged with. The after set is this branch. Both are deterministic and 20 Hz.
The pulse sweep and the sword-reach stander below were one-off measurements run in temporary
test files that are not committed; their method is described with each table.

## Latency room for ritual pulses

**How the intervals were chosen.** Each ritual's shown interval was swept with the other two
rituals left at their old values: the king held at stage 2 (burial, edict) or 3 (requiem) for
300 s against evaders — solo melee, solo ranged, and four with two ranged — at each network
condition. Hits on that ritual, summed over the three setups:

| Pulse | 0 ms | 250 ms | 400 ms | 600 ms | 4-in-20 loss | 250 ms and loss |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Burial 700 ms | 0 | 0 | 53 | 80 | 0 | 5 |
| Burial 900 ms | 0 | 0 | 0 | 51 | 0 | 0 |
| **Burial 1.0 s** | 0 | 0 | 0 | 0 | 0 | 0 |
| Edict 800 ms | 0 | 3 | 2 | 6 | 1 | 5 |
| Edict 1.0 s | 0 | 0 | 4 | 3 | 0 | 0 |
| Edict 1.1 s | 0 | 0 | 1 | 5 | 0 | 1 |
| Edict 1.2 s | 0 | 0 | 0 | 5 | 0 | 0 |
| **Edict 1.3 s** | 0 | 0 | 0 | 0 | 0 | 0 |
| Requiem 900 ms | 0 | 1 | 6 | 15 | 1 | 6 |
| Requiem 1.0 s | 0 | 1 | 1 | 5 | 0 | 2 |
| Requiem 1.1 s | 0 | 0 | 0 | 5 | 0 | 0 |
| Requiem 1.2 s | 0 | 0 | 0 | 1 | 0 | 0 |
| **Requiem 1.3 s** | 0 | 0 | 0 | 0 | 0 | 0 |

The chosen interval is the shortest one with no hit through 600 ms. Clean through 400 ms alone
would have been 900 ms, 1.2 s and 1.1 s. The extra step is margin for what the harness does not
model: a starving player walks at 80% speed.

**The full matrix, before and after.** Coverage fights with evaders, both ritual stages:
escaped over threatened windows, then the blows that ritual landed.

| Ritual | 0 ms | 250 ms | 400 ms | 4-in-20 loss |
| --- | --- | --- | --- | --- |
| Burial | 300/300, 0 → 266/266, 0 | 300/302, 2 → 271/271, 0 | **83/301, 218 → 265/265, 0** | 299/300, 1 → 268/268, 0 |
| Edict | 79/79, 0 → 66/66, 0 | 75/79, 4 → 69/69, 0 | 85/87, 2 → 72/72, 0 | 77/78, 1 → 69/69, 0 |
| Requiem | 71/71, 0 → 72/72, 0 | 69/70, 1 → 72/72, 0 | 64/70, 6 → 67/67, 0 | 70/71, 1 → 70/70, 0 |

Natural fights, solo readers against the king: 400 ms went from 1 hit (rusty) and 1 (iron) to
0 and 0; 500 ms from 2 and 1 to 0 and 0; 600 ms from 3 and 2 to 0 and 0; 250 ms with loss from 1
to 0. At 800 ms, 3 and 2 hits remain, none before perception (2 and 1 were before perception).
The Vargr fights were clean before and after.

`TestFirstDungeonRitualPulsesSurviveLatency` pins zero ritual pulse hits at 400 ms and at 250 ms
with loss, and `TestFirstDungeonReadersEscapeEveryAnnouncedRegion` now includes 400 ms.

## Strike reach

**The ceiling.** A player strikes a boss from anywhere its axis-aligned body gap is within
`SwordReach` (2.5). Swept over bearings in tenths of a degree, the farthest the player's nearest
damage sample then stands from the boss's centre is 3.631 blocks for the Vargr and 3.207 for the
Draugr, both on the 45° diagonal (front-on: 3.314 and 3.015). Reach beyond that, rounded up to a
tenth, damages players who cannot be striking back, and no engagement explains it.
`TestPlantedBlowsReachNoFartherThanTheirEngagement` pins every planted blow under its boss's
ceiling.

| Move | Before | After | Engagement ceiling | Visible strike | Far boundary past the visible strike, after |
| --- | ---: | ---: | ---: | ---: | ---: |
| Vargr bite and tear | 3.0 | 3.0 | 3.7 | 0.885 / 0.893 | 2.115 / 2.107 |
| Vargr prisoner claws | 3.4 | 3.4 | 3.7 | 0.978 | 2.422 |
| Vargr bonebreaker jaws | 3.6 | 3.6 | 3.7 | 0.834 | 2.766 |
| Draugr king's sentence | 5.0 | 3.3 | 3.3 | 1.520 | 1.780 (was 3.480) |
| Draugr first / second toll | 3.8 | 3.3 | 3.3 | 1.567 / 1.560 | 1.733 / 1.740 (were 2.233 / 2.240) |
| Draugr third toll | 3.8 | 3.3 | 3.3 | 1.740 | 1.560 (was 2.060) |

The visible strikes are those measured in [#1028](vargr-choreography-1028.md) and
[#1034](draugr-choreography-1034.md). **Every Vargr boundary is within the ground a player can
strike it from, so none was cut**: its remaining gap to the visible fang and claw is engagement
reach, and closing it visually is presentation, which is part 3. No blow damages outside its
announced region, before or after.

**Selection.** A body-to-body band and a centre-anchored region do not describe the same ground:
the bite's 2.6 band admits targets its 3.0 cone cannot reach, and a diagonal gap reaches farther
than a front one. One-off measurement: a solo iron stander holding 2.45 blocks from the boss's
body, spawned 4.5 blocks out on clear floor, 60 s. "Missed" counts planted moves whose announced
region did not reach it on the tick they were announced.

| Boss, spawn bearing | Before: planted, missed, hits | After: planted, missed, hits |
| --- | --- | --- |
| Vargr 90°, 225°, 315° | 4, 3, 1–2 | 4, 0, 4 |
| Draugr 90°, 45°, 135°, 225°, 315° | 5, 0, 6 | 5, 0, 6 |

The Vargr's 45° and 135° spawns overlap terrain in the shipped chamber and were skipped rather
than moved. `TestAPlantedMoveIsChosenOnlyWhereItsRegionReachesTheTarget` pins the rule: a target
on the Vargr's diagonal at sword reach gets no planted move, while the same gap in front does.

## Side effects, not tuning

Natural fights at zero delay, readers: kill times with stage changes in brackets.

| Fight | Before | After |
| --- | --- | --- |
| Draugr, solo, rusty | 38.75 [16.20, 27.25] | 41.70 [18.50, 31.30] |
| Draugr, solo, iron (leather or iron) | 26.30 [10.95, 19.10] | 26.05 [12.40, 19.55] |
| Draugr, two, rusty | 16.95 [6.20, 11.75] | 17.35 [6.85, 12.15] |
| Draugr, two, iron | 11.10 [4.25, 7.85] | 11.50 [4.25, 8.25] |

Every other zero-delay reader fight kept its kill time to the hundredth, including every Vargr
fight and every Draugr fight of three or four players. Under delay, solo readers against the
Vargr now kill it sooner — measured here, not explained:

| Solo reader against the Vargr | Rusty, before → after | Iron, before → after |
| --- | --- | --- |
| 400 ms | 32.90 → 28.10 | 19.55 → 17.50 |
| 500 ms | 33.00 → 28.20 | 19.65 → 17.60 |
| 600 ms | 33.10 → 28.30 | 19.85 → 17.55 |
| 800 ms | 33.30 → 28.50 | 20.05 → 17.75 |
| 250 ms and loss | 32.95 → 31.40 | unchanged |

The Draugr's delayed fights moved as well, as the latency section's hit counts reflect. Of the numbers the part
1 record states, the stander table changed where a corrected value is the subject: two fewer
sentence blows (65 → 63) and the burial pulse's warning and promise rising from 13/13 to 19/19
ticks. The solo rusty stander against the Draugr now dies 37 times in 900 s instead of 38, with
37 wipes. The part 1 escape-path and latency tables are the baseline this record compares with.

Longer rituals leave the requiem channelled for 3.9 s instead of 2.7 s, which makes its
150-damage interrupt easier to reach. The interrupt remains unmeasured, as part 1 recorded.

## Remaining limits

- Outside the rituals, a few blows still land under delay on evaders:
  - prisoner claws: 2 blows at 250 ms and 2 at 400 ms, against 1 and 4 before;
  - three tolls: 1 blow at 400 ms, as before.

  These are the non-ritual limits and are not changed here.
- At 800 ms, natural fights still take hits; a region can arrive after the shown interval.
- The selection rule is checked when a move is announced. A target that moves afterwards can
  still leave the region, which is what reading is.
- Engagement reach is a geometric ceiling from `SwordReach` and body boxes. It is not a claim
  that the Vargr's fang is visually fair; part 3 owns presentation and GPU captures.
- 20 Hz, full hunger and scripted policies, as in part 1.

## Validation

`gofmt`, `go vet`, golangci-lint, `go build`, the 386 and arm builds and `go test ./...` passed,
as did the automation shell suite and the DeepSeek Python tests. In the after matrix, no
zero-delay blow is premature and no health loss is unattributed.
