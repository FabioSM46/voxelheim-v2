# First dungeon: level-21 healer comparison (#1361)

The first attempt exposed a bot replacement defect; the same-build repeat after its repair
is recorded in [Repeat after the bot repair](#repeat-after-the-bot-repair-1368).
Both attempts are retained so the failed evidence is not lost.

## First attempt: preserved evidence before #1368

The first four attempts used one exact source, one world seed and one starting level.
Both healer attempts stopped in the guardian phase at the first sceptre replacement;
[bug #1368](https://github.com/FabioSM46/voxelheim-v2/issues/1368) records the
production inventory rule the bot violated. These are failed acceptance attempts, not
completed healer clears and not evidence of a balance regression. Missing kill times and
human clear estimates are unavailable, not zero.

The comparison follows [the #1333 group record](dungeon-descent-1298.md#group-runs-1333).
Level **21** is intentional: `server/internal/game/dungeon_balance.go` prices the lesser
creatures' doubled damage against the level-21 health pool. It avoids treating a level-1
party, deliberately below the dungeon tier, as the intended survivor. Characters start at
21 through one `/addexperience` each; ordinary earned experience is not disabled. Boss
health scales with party size, while damage answers levels at the pull. One healer replaces
one iron-blade member and carries a wooden sceptre; all members wear rusty armour.

Measured on 2026-09-29, exact clean source
`211f72c2443a6b337d77da295c9c422521554d7c` (includes #1358, #1359 and #1360).
The server and bot were each built once with Go 1.26.6, Linux amd64, then reused unchanged:

| Binary | SHA-256 |
| --- | --- |
| `voxelheimd` | `3fca0fe2de4fa2fb773e555579ce2e82c2a1baa40133f6ee3332dc3b6abcce58` |
| `descentbot` | `b69eb0b876c7b78484386196fc33dec248bd29968288ee35ec314a17ba3e1d1a` |

Four independent bot/server pairs began together at 10:33 UTC on a shared 16-thread,
approximately 31-GiB machine, with about 14 GiB available. Each owned a fresh temporary
world, signing key and kernel-selected loopback listener. Startup one/five/fifteen-minute
load averages were 4.64/5.21/5.03; the one-minute load reached 14.17 while full server tests
also ran, and fell to 6.08 after the healer failures. Automation tests also overlapped
startup. These are shared-machine wall clocks, not an isolated latency benchmark or repeated
statistical sample. Reports do not measure server tick lateness; no correction for machine
load is asserted.

All four requests use **world seed 1**, view distance 4, default maximum three consecutive
wipes, timeout 60 minutes. The instance seed is derived from instance allocation and is
**not identical across party sizes**: 5306321562182508718 for three and
5306321562182508716 for five. Within each party size it is identical with/without a healer,
and these are also the seeds reported by #1333. Thus the role comparison is paired by
instance seed; comparing sizes also compares those two rotations.

From `<repo-root>/server`:

```sh
go build -o <output-directory>/voxelheimd ./cmd/voxelheimd
go build -o <output-directory>/descentbot ./cmd/voxelheim-descentbot
<output-directory>/descentbot -server <output-directory>/voxelheimd -seed 1 -party 3 -healers 0 -level 21 -timeout 60m
<output-directory>/descentbot -server <output-directory>/voxelheimd -seed 1 -party 3 -healers 1 -level 21 -timeout 60m
<output-directory>/descentbot -server <output-directory>/voxelheimd -seed 1 -party 5 -healers 0 -level 21 -timeout 60m
<output-directory>/descentbot -server <output-directory>/voxelheimd -seed 1 -party 5 -healers 1 -level 21 -timeout 60m
```

The measured commands were launched concurrently; the lines above specify each invocation.
The bot enables development commands on its isolated server. No hoard scenario was enabled.

## Completed all-blade controls

| Measurement | Three, no healer | Five, no healer |
| --- | ---: | ---: |
| Result | cleared portal to portal | cleared portal to portal |
| Portal-to-portal clock / empirical human proxy | **21.6 min, inside 17–23** | **21.5 min, inside 17–23** |
| Historical reader-replacement estimate | **21.5 min, inside 17–23** | **21.4 min, inside 17–23** |
| Guardian first pull to kill | 204.7 s | 205.3 s |
| King first pull to kill | 329.5 s | 329.1 s |
| Deaths per member | 1, 1, 1 | 1, 1, 1, 1, 1 |
| Wipes, all parts | 0 | 0 |
| Healer requests / inferred restoration | not applicable: no healer | not applicable: no healer |
| `/additem`, initial equipment | 12 | 20 |
| `/addexperience` | 3 | 5 |
| `/teleport`, portal placements | 6 | 10 |
| `/teleport`, stuck assists | 2 | 1 |
| `/immortal` | **0** | **0** |
| All development commands | **23** | **36** |

The stuck assists were all at the timed grille: Hild and Orm at three, Orm at five.
These clears are assisted acceptance runs, not proof of an unassisted human clear.
No sceptre replacements occur in the all-blade controls. The commands in the tables
exhaust the reported total: there are no unclassified development commands.

| Part | Three: wall clock | Five: wall clock | Deaths / wipes |
| --- | ---: | ---: | --- |
| Upper halls | 61.9 s | 57.3 s | [1,1,1] / 0; [1,1,1,1,1] / 0 |
| Rune hall | 11.1 s | 12.0 s | all zero, both parties |
| Guardian | 207.8 s | 208.4 s | all zero, both parties |
| Chasm and pool | 12.8 s | 9.6 s | all zero, both parties |
| Cave waves | 503.7 s | 504.3 s | all zero, both parties |
| Web curtain | 8.9 s | 9.6 s | all zero, both parties |
| Timed grille | 23.0 s | 26.6 s | all zero, both parties |
| Sand hall | 43.1 s | 43.2 s | all zero, both parties |
| Twin levers | 15.2 s | 15.2 s | all zero, both parties |
| King | 340.6 s | 339.9 s | all zero, both parties |
| Return shortcut | 53.0 s | 52.4 s | all zero, both parties |

Part duration includes movement and waiting for the last member, so it differs from
first-pull-to-kill time. The total includes portal approach/entry outside the part table;
rounded part times need not sum to the displayed rounded total.

The three-member control killed six draugr, six vargr, 36 spiders, three scorpions and both
bosses; five killed ten draugr, ten vargr, 60 spiders, five scorpions and both bosses.
Both cut 15 webs, matched the rune inscription to the instance order, passed grille
lever-to-checkpoint in 9.2/9.1 seconds, and completed twin levers in 7.5 seconds.

## Failed healer attempts

| Measurement | Three, one healer | Five, one healer |
| --- | ---: | ---: |
| Result | failed in guardian | failed in guardian |
| Portal-to-portal clear | unavailable: did not exit | unavailable: did not exit |
| Guardian first pull to kill | unavailable: not killed | unavailable: not killed |
| King first pull to kill | unavailable: not reached | unavailable: not reached |
| Deaths by member order | 2, 1, 2 | 1, 2, 2, 2, 0 |
| Whole-party deaths | 5 | 7 |
| Wipes, all reached parts | 0 | 0 |
| Healer (last member) | Orm | Ulf |
| Ally orb requests | 19 | 11 |
| Creature orb requests | 18 | 39 |
| Total orb requests | 37 | 50 |
| Positive health deltas near own ally requests (inferred) | 317 | 63 |
| Confirmed sceptre replacements | 0 | 0 |
| Replacement `/additem` attempts | 1 | 1 |
| `/additem`, including initial equipment | 13 | 21 |
| `/addexperience` | 3 | 5 |
| `/teleport`, portal placements | 6 | 10 |
| `/teleport`, stuck assists | 0 | 0 |
| `/immortal` | **0** | **0** |
| All development commands | **22** | **36** |

Member order is Delver, Hild, Orm, Sigrun, Ulf, truncated to party size. These are the
harness's synthetic character names. The extra `/additem` is counted even though the
replacement was not equipped: zero completed replacements does not mean zero assistance.
A fresh sceptre lasts 50 launches, but death also wears equipment; the three-member
healer died twice before exhausting the sceptre after only 37 recorded requests.

| Part | Three: time / deaths / wipes | Five: time / deaths / wipes |
| --- | --- | --- |
| Upper halls | 82.2 s / [2,1,2] / 0 | 73.8 s / [1,2,2,2,0] / 0 |
| Rune hall | 11.3 s / [0,0,0] / 0 | 11.9 s / [0,0,0,0,0] / 0 |
| Guardian | interrupted; clock unavailable / [0,0,0] / 0 | interrupted; clock unavailable / [0,0,0,0,0] / 0 |
| All later parts | not reached | not reached |

The bot emits duration only for a completed part and emits total duration only for a
successful clear. Its progress log proves entry into guardian and the terminal failure,
but does not report a first-pull-to-failure measurement. No duration has been invented for
that missing field. Three members killed six draugr and six vargr; five killed ten of each.
Both killed zero bosses. Neither failure is a wipe-limit termination.

**Healing evidence is limited by the protocol.** Counts are successfully written launch
requests, which the server may refuse, not confirmed orbs. Restoration is positive health
change observed on the intended living ally inside a bounded flight-time window. Passive
regeneration can contribute, damage between snapshots can conceal healing, and the wire
does not attribute a heal to its source. Even with only one healer these numbers are not
confirmed sceptre healing, and they are not estimates of healing efficiency.

**The replacement fails for a deterministic reason.** `ensureSceptre` sends a fresh
same-ID sceptre directly onto the worn main hand. `inventory.moveLocked` rejects same-item
equipment/durable moves before its different-item swap branch. The bot then times out
waiting for authoritative confirmation. Its fixture had manually fabricated that swap,
which explains why unit tests pass while both real sessions fail. #1368 requests a bot
repair and a production-backed regression; no authoritative inventory rule is changed here.

## Upper halls compared with level 1

The #1333 level-1 runs wiped **twice** in the upper halls at both party sizes, with
death vectors [2,2,3] and [2,2,2,2,2]. The level-21 controls had one death per member and **zero wipes**, down from the old
seven/ten deaths and two wipes. Both level-21 healer attempts completed the
halls with **zero wipes**, but still lost five and seven lives respectively. Surviving
without a whole-party reset is an improvement over that historical record; it does not
show that nobody dies, or isolate a causal healer benefit. Level, the new role, harness
revision and machine conditions differ from the earlier measurement. At the same level
and instance seed, the healer attempts actually had more upper-hall deaths than their
all-blade controls (5 versus 3, and 7 versus 5). One attempt per composition is too little
to claim that healing causes this difference; it certainly does not establish a survival benefit.

## Band assessment and acceptance limits

A completed healer run is required before judging its 17–23-minute band. Both healer
estimates are **unavailable**, with no defensible finite overrun or part attribution: their
route was interrupted by a harness defect. Replacing their missing fights with historical
all-blade times would conceal the exact composition question this measurement was meant
to answer. No healer balance issue is filed because no healer clear was measured outside
the band; #1368 is the concrete acceptance blocker.

For the completed controls, the unmodified observed wall clock is the empirical proxy for
a human party executing the same route, dodges, waits, deaths and assistance. That is a
conditional comparison, not a calibrated prediction for human players. The bot's historical
estimate is separately identified: it swaps both fights for level-1 iron-reader baselines,
206.65/324.70 seconds for three and 204.55/324.70 for five. Both raw and historical
estimates remain inside the band: their displayed margins below 23 minutes are 1.4/1.5
minutes (raw) and 1.5/1.6 minutes (historical), respectively. Neither has an out-of-band
part to attribute. It does not calibrate a healer
composition, and it is not used to fill the failed runs.

## Validation

The complete `go test ./...` in `server/` passed. An uncached
`go test ./internal/game -run '^TestTheRouteTakesSeventeenToTwentyThreeMinutes$' -count=1 -v`
also passed. The latter is the historical all-blade route model, not a live-healer test.
Every `scripts/test/*.test.sh` and `.github/scripts/test_deepseek_review.py` passed.
The exact source commit also passed [Integration run 36556132624](https://github.com/FabioSM46/voxelheim-v2/actions/runs/36556132624),
including `integration-verdict`. Those green gates did not exercise this real inventory
replacement; the live failures show the limit of that coverage.


Publication/body privacy scans and `git diff --check` passed on the final local record.
No gameplay, bot, client, schema or balance file is changed by this record. The owner
authorized the bot-only #1368 repair and a complete four-run repeat on one new build.
The results above remain the first attempt and are not replaced by that later comparison.

## First-attempt acceptance limits

- Four attempts on the same build/world seed/starting level are recorded, including failures.
- Deaths, wipes, development commands, healer request counts and inferred health deltas are retained.
- Both control clears and both boss times are measured; both healer clears and boss times are missing.
- A healer human-clear band verdict and its possible overrun/part attribution remain unavailable.
- Launch confirmation and source-attributed healing are not exposed by this protocol; the report
  uses the explicitly limited inference provided by #1360.
- This first attempt alone does not complete #1361. The newly authorized #1368 repair and
  a complete four-run repeat must provide the missing evidence before acceptance closure.

## Repeat after the bot repair (#1368)

The owner authorized the narrowly scoped replacement repair and the complete four-run
repeat. [PR #1369](https://github.com/FabioSM46/voxelheim-v2/pull/1369) changed only the
bot and its tests. The final comparison uses exact source
`984126aaee0da41a07eeb9328c268783e2f7c231` throughout; no measured server or bot source
was edited between compositions. The first attempt above remains intact.

The single server build has SHA-256
`3fca0fe2de4fa2fb773e555579ce2e82c2a1baa40133f6ee3332dc3b6abcce58`, identical to the
first attempt, while the repaired bot has SHA-256
`aef5d8ddfad7146f32f5d85cce2e8f90da925e6b4ad2be004db5d9e1af8bb6c2`.
Both were built with Go 1.26.6 for Linux amd64. The same four command lines above were
run again with these binaries, world seed 1, level 21 and unchanged remaining flags.

The full server tests, the uncached route-band test and the unconditional automation suite
all passed **before** starting the repeat. The exact source also passed
[Integration run 36569298771](https://github.com/FabioSM46/voxelheim-v2/actions/runs/36569298771),
including `integration-verdict`, before launch. No local test or build overlapped this repeat.
Four separate bot/server pairs started together at 12:47:45 UTC on 2026-09-29 on the same
shared machine. Initial one/five/fifteen-minute load averages were 1.26/1.77/3.14 with about
16.4 GiB of available memory. Concurrency and external workload can still affect wall time;
this repeat is not an isolated benchmark or a calibrated prediction of human performance.

Sampled one-minute load during the repeat ranged from 1.34 to 6.40. These are periodic
samples, not a claim about the unsampled maximum. Both derived instance seeds remained the
same as in the first attempt, paired by party size.

### Final result and all development assistance

All four repeated runs **cleared portal to portal** and completed clean server shutdown.
The healer replacement repair completed ten replacements at three and nine at five.

| Measurement | 3, no healer | 3, one healer | 5, no healer | 5, one healer |
| --- | ---: | ---: | ---: | ---: |
| Portal-to-portal wall clock | **21.4 min** | **26.0 min** | **21.8 min** | **23.8 min** |
| Guardian, first pull to kill | 206.5 s | 284.7 s | 205.7 s | 248.2 s |
| King, first pull to kill | 331.0 s | 469.2 s | 343.4 s | 404.7 s |
| Deaths by member order | [1,1,1] | [2,1,1] | [0,0,1,1,1] | [1,1,1,1,1] |
| Total deaths | 3 | 4 | 3 | 5 |
| Total wipes | 1 | 0 | 0 | 0 |
| Healer | none | Orm | none | Ulf |
| Ally launch requests | — | 33 | — | 17 |
| Creature launch requests | — | 475 | — | 439 |
| Total launch requests | — | 508 | — | 456 |
| Inferred positive health points near own ally requests | — | 465 | — | 263 |
| Confirmed worn sceptre replacements | 0 | 10 | 0 | 9 |
| `/additem`, initial kit | 12 | 12 | 20 | 20 |
| `/additem`, replacements | 0 | 10 | 0 | 9 |
| `/addexperience` | 3 | 3 | 5 | 5 |
| `/teleport`, portal placements | 6 | 6 | 10 | 10 |
| `/teleport`, stuck assists | 2 | 1 | 2 | 0 |
| `/immortal` | **0** | **0** | **0** | **0** |
| All development commands | **23** | **32** | **37** | **44** |

No development command is omitted from the totals. All stuck assists occurred at the
timed grille: Hild and Orm for three blades, Hild for the three-member healer party,
Orm and Ulf for five blades. Replacement `/additem` requests and confirmed replacements
match in this repeat; the failed first attempt above separately counts uncompleted grants.
The worn sceptres were retained, not silently repaired or discarded. This replenishment
is substantial development assistance, not evidence about normal weapon-resource economy.

The request and restoration limitations described in the first attempt apply unchanged:
**508/456 are requests, not confirmed orb launches; 465/263 are inferred stream deltas,
not source-attributed sceptre healing.** A protocol-level confirmed measurement is unavailable.

### Final route parts

The times below are party wall clocks in seconds. In all four runs every death occurred
in the upper halls; all later parts have zero deaths for every member and zero wipes.
The only wipe was the three-blade party's one upper-hall wipe. The first row includes
the complete death vectors and wipe counts; that explicit zero rule applies to every
subsequent row, including both bosses.

| Part | 3, no healer | 3, one healer | 5, no healer | 5, one healer |
| --- | ---: | ---: | ---: | ---: |
| upper halls | 59.0 s; [1,1,1] deaths; 1 wipes | 74.9 s; [2,1,1] deaths; 0 wipes | 51.9 s; [0,0,1,1,1] deaths; 0 wipes | 70.3 s; [1,1,1,1,1] deaths; 0 wipes |
| rune hall | 11.2 s | 11.0 s | 11.9 s | 11.8 s |
| guardian | 209.7 s | 287.9 s | 208.8 s | 251.3 s |
| chasm and pool | 12.9 s | 13.6 s | 9.6 s | 11.2 s |
| cave waves | 504.2 s | 503.2 s | 505.1 s | 503.4 s |
| web curtain | 8.9 s | 8.8 s | 11.0 s | 8.6 s |
| timed grille | 23.0 s | 23.0 s | 26.8 s | 15.3 s |
| sand hall | 40.9 s | 77.1 s | 45.5 s | 63.3 s |
| twin levers | 14.8 s | 14.9 s | 18.5 s | 15.0 s |
| king | 342.2 s | 480.2 s | 354.5 s | 415.6 s |
| return shortcut | 44.5 s | 56.3 s | 53.7 s | 53.1 s |

Both three-member parties killed six draugr, six vargr, 36 spiders, three scorpions and
both bosses. Both five-member parties killed ten draugr, ten vargr, 60 spiders, five
scorpions and both bosses. Each cut all 15 webs and read the correct rune order.

| Puzzle timing | 3, no healer | 3, one healer | 5, no healer | 5, one healer |
| --- | ---: | ---: | ---: | ---: |
| Grille lever to checkpoint | 9.1 s | 9.1 s | 11.2 s | 9.0 s |
| Twin levers | 7.5 s | 7.5 s | 9.4 s | 7.5 s |

### Final human-time proxy and band finding

For **every composition**, this record uses `estimated human proxy = measured raw
portal-to-portal clock`, conditional on a human party reproducing the bot's route,
combat policy, waits, deaths and recorded assistance. No boss duration is replaced.
This is a reproducible empirical comparison, **not a calibrated estimate of typical
human performance**. Skilled or inexperienced players, different healing policies,
normal repair economy and unassisted navigation can change the result in either direction.
One final sample per composition supplies no confidence interval or causal isolation.

| Composition | Conditional human proxy | 17–23-minute verdict |
| --- | ---: | --- |
| Three, no healer | 21.4 min | inside; 1.6 min below upper edge |
| Three, one healer | 26.0 min | **outside; 3.0 min above upper edge** |
| Five, no healer | 21.8 min | inside; 1.2 min below upper edge |
| Five, one healer | 23.8 min | **outside; 0.8 min above upper edge** |

Minutes and margins use the report's one-decimal precision. The bot's historical
reader-replacement estimates for controls are separately 21.3 and 21.5 minutes. The bot
correctly reports that estimate as unavailable for healers, because no corresponding
healer-reader baseline exists. The conditional proxy here does not pretend to fill that
calibration gap or modify the bot's output.

The measured slowdown is concentrated in **the bosses**, with smaller upper- and
sand-hall increases. Relative to its same-size all-blade control:

| Additional measured time with healer | Three | Five |
| --- | ---: | ---: |
| Guardian first pull to kill | +78.2 s | +42.5 s |
| King first pull to kill | +138.2 s | +61.3 s |
| Both boss fights | **+216.4 s** | **+103.8 s** |
| Sand hall | +36.2 s | +17.8 s |
| Upper halls | +15.9 s | +18.4 s |
| Cave waves | -1.0 s | -1.7 s |
| Entire run, rounded clocks | +4.6 min | +2.0 min |

Boss increases alone consume about 3.61 minutes at three and 1.73 minutes at five,
against control headroom of about 1.6 and 1.2 minutes. This locates the measured overrun
without inventing a separate budget for each part. Other route deltas, assistance and
rounding explain the remaining total difference. No healer died or wiped at either boss.
The comparison is evidence of slower assisted role-composition runs, not proof that
healing itself causes the entire difference.

Filed [#1370](https://github.com/FabioSM46/voxelheim-v2/issues/1370) with the same-build
evidence, limitations and reproduction commands, as #1361 requires when a healer run
leaves the band. It requests role-aware calibration and balance investigation; no balance
number is changed here and no new iteration membership is assigned by this record.
The historical `TestTheRouteTakesSeventeenToTwentyThreeMinutes` remains green because
it models all-blade readers, not these healer compositions.

### Final comparison with the historical upper halls

At level 1, #1333 reported two upper-hall wipes for each party size and seven/ten deaths.
The level-21 repeat reports **one/zero wipes without a healer**, and **zero/zero with one**,
for three/five respectively. Death counts are three/three without and four/five with a
healer. Both compositions improve on the historical wipe count, but this single paired
sample does not establish a healer survival benefit: healer deaths remain higher than
the same-level controls, and the earlier first-attempt controls also show
run-to-run variability. Neither level nor role is isolated against the older harness.

### Final validation and scope

All four required final live runs completed on one exact build/world seed/starting level.
Both boss kills, every part's deaths/wipes, all commands and the available healer evidence
are reported. Human-band classification uses the explicitly conditional raw-time proxy;
source-attributed healing and calibrated human timing remain unavailable, not fabricated.

The full server test suite, uncached route-band test, automation helper suite and DeepSeek
review-script tests passed on the final source before live runs. Publication/body privacy
and diff checks passed on the completed record; commit privacy is checked before push.
Only this document and a link from the older record change. The first failed attempts,
#1368 diagnosis and repaired-run overrun finding remain visible.
