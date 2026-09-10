# First dungeon combat measurement, part 1 (#1037)

This record is the baseline measured before any correction. Part 2 changed the ritual pulse
intervals, the Draugr's strike reach and move selection; its before and after numbers, and the
values below that moved, are recorded in [dungeon-corrections-1037.md](dungeon-corrections-1037.md).

This part measures the assembled fights and changes no rule. Every number below comes
from the production simulation of the shipped dungeon, at the default 20 Hz, and every
tuning question it raises is handed to the parts that follow rather than settled here.

| Part | Surface | Status |
| --- | --- | --- |
| 1 | Server combat measurement harness and this baseline | this pull request |
| 2 | Server corrections for the findings below, measured before and after | next |
| 3–5 | Client contact presentation and GPU captures; mixer exports; rendering and server cost | later |

## What was run

`server/internal/game/encounter_playtest_test.go` builds the real instance: the shipped
chamber, both placed bosses, the production scheduler, escape check, collision, armour and
damage, stepped by `InstanceManager.Step`. Up to four scripted players join through
`Sim.JoinCharacter` with a stored life. Each player perceives the fight **only through the
frames its own session is delivered** — `EntitySnapshot` and `EncounterTimeline`, decoded
from bytes — after a configurable delay and burst loss. It answers with `Player.Submit` and
`Player.Attack`. The scheduler is read only afterwards, by the referee.

- **Policies.** A *reader* walks straight out of every region it believes is announced, by
  the bearing that clears soonest under the player's own collision and step-up. It keeps a
  0.35-block margin and swings whenever it is in reach and clear. A *stander* closes to the
  same gap and never moves again; it is the negative control. An *evader* reads like a reader
  and never swings, so a boss held at one stage runs its whole repertoire.
- **Positions.** Melee players hold a 2.0-block body gap. A *ranged* member holds 6.0 and backs
  away from pursuit, which the charge, leap and spear need before they can be chosen.
- **Entry kits.** The rusty sword with nothing worn, the iron sword in leather, and the iron
  sword in iron armour, at level 1 and full hunger, with no shield raised.
- **Network.** Delay holds every frame; burst loss delivers nothing for four ticks in every
  twenty. The delay stands for latency and reaction together.
- **Stages.** Natural fights run from the pull to the kill. Coverage fights pull by hand at
  the health a stage begins at and run 300 seconds against evaders.

A scripted player is a policy, not a person. Zero delay is perfect reflexes.

## Reproduction

From `<repo-root>/server`:

```sh
go test ./internal/game -run 'TestFirstDungeon(Readers|EveryStage|Damage)' -v
VOXELHEIM_PLAYTEST=1 VOXELHEIM_PLAYTEST_DIR=<review-output> \
    go test ./internal/game -run TestFirstDungeonPlaytest -v
```

The first line is always run by `go test ./...`. The second writes
`dungeon-combat-1037-fights.csv` (182 fights) and `dungeon-combat-1037-moves.csv` (664 move
rows). The simulation is deterministic: two consecutive runs on 2026-09-10 produced
byte-identical files, so the CSVs are regenerated rather than committed. The whole matrix took
49.1 s of wall time on an AMD Ryzen 7 3700X, x86_64 Linux. Every duration below is simulated
time and does not depend on the host.

## Kill times

Seconds from the pull to the kill, readers at zero delay. Brackets give the stage changes.
"Recovery share" is the part of the boss's health the party removed while it recovered.

| Boss | Kit | 1 player | 2 players | 3 players | 4 players |
| --- | --- | ---: | ---: | ---: | ---: |
| Vargr | rusty, unarmoured | 32.50 [14.10] | 12.45 [6.10] | 9.40 [4.00] | 5.85 [3.05] |
| Vargr | iron, leather or iron | 19.15 [10.05] | 7.80 [4.55] | 5.95 [2.70] | 3.90 [2.40] |
| Draugr | rusty, unarmoured | 38.75 [16.20, 27.25] | 16.95 [6.20, 11.75] | 11.75 [4.15, 8.30] | 8.50 [3.30, 5.90] |
| Draugr | iron, leather or iron | 26.30 [10.95, 19.10] | 11.10 [4.25, 7.85] | 7.40 [2.85, 5.25] | 5.90 [2.00, 3.95] |

Recovery share, solo: Vargr 94% (rusty) and 89% (iron); Draugr 56% and 57%. It falls with
party size, to 42–50% for four Vargr players and 17–27% for four Draugr players; the damage a
party lands during telegraphs and releases rises correspondingly.

Leather and iron readers kill at identical times because they swing the same blade and take
no damage. The stander shows the armour: a solo iron stander kills the Vargr in 11.70 s
taking 74 damage, 90 in leather. A solo rusty, unarmoured stander never kills either boss
in 900 s: 39 deaths and 39 wipes against the Vargr, 38 and 38 against the Draugr.

**The approved design asks for 3–4 minutes against the Vargr and 5–6 against the Draugr,
and every measured fight is shorter by a factor of 5 or more.** The design also says the
length must not come from inflating health, and the server has no party-size adaptation.
No health, damage or scaling was changed. The repository owner decided that #1037 records this
gap and no #1037 part tunes fight length; balance is #1099, where the boss scales with the
levels of the party members at the pull, measured with this harness.

## Telegraphs precede damage

Every stander blow, measured from the tick the struck player first perceived that region and
checked against the promise of its own phase: the telegraph for a release, or the last tick of
the shown interval for a pulse. A blow is charged to a move only when that move's own hit
ledger gained the struck player on that tick — including the blow that wipes the pull, which
the server's reset replaces the boss inside. No health loss in any measured fight was left
unaccounted for.

| Move | Blows | Fewest ticks perceived before the blow | Promised |
| --- | ---: | ---: | ---: |
| Vargr bite and tear | 166 | 19 | 18 |
| Vargr prisoner claws | 45 | 21 | 20 |
| Vargr predator leap | 12 | 32 | 20 |
| Vargr bonebreaker jaws | 17 | 31 | 30 |
| Draugr king's sentence | 65 | 25 | 24 |
| Draugr three tolls | 114 | 19 | 18 |
| Draugr sepulchre spear | 12 | 30 | 28 |
| Draugr burial pulse | 31 | 13 | 13 |

No blow came sooner than its promise, which
`TestFirstDungeonDamageNeverPrecedesItsPerceivedAnnouncement` pins, and
`TestFirstDungeonPlaytestRefereeCatchesWhatItClaims` proves the check can fail: a telegraph or
pulse cut short after its announcement is reported as premature, and a loss no ledger accounts
for is kept out of every move. No stander was struck by a
collar charge, an edict or a requiem pulse in these natural fights; the coverage fights below
measure those against evaders.

## Escape paths

Coverage fights, evaders at zero delay: windows whose region covered a player's body on the
tick it was announced, and how many never struck. The longest escape is the longest
continuous walk while inside any believed region; it can span consecutive announcements.

| Move | Announced ticks | Moves | Escaped / threatened | Longest escape walk |
| --- | ---: | ---: | ---: | ---: |
| Vargr bite and tear | 18 | 370 | 508 / 508 | 10 |
| Vargr prisoner claws, single and paired | 20 | 266 | 375 / 375 | 19 |
| Vargr collar charge | 24 | 41 | 41 / 41 | 11 |
| Vargr predator leap | 20 | 48 | 48 / 48 | 17 |
| Vargr bonebreaker jaws | 30 | 84 | 97 / 97 | 11 |
| Draugr king's sentence | 24 | 205 | 303 / 303 | 11 |
| Draugr three tolls | 18 each | 526 | 824 / 824 | 11 |
| Draugr sepulchre spear | 28 | 77 | 77 / 77 | 9 |
| Draugr burial, per pulse | 14 | 88 | 300 / 300 | 13 |
| Draugr edict of the graves, per pulse | 16 | 85 | 79 / 79 | 14 |
| Draugr requiem of the buried, per pulse | 18 | 40 | 71 / 71 | 18 |

Natural fights agree: readers at zero delay were never struck in any of the 24 fights, and
`TestFirstDungeonReadersEscapeEveryAnnouncedRegion` and
`TestFirstDungeonEveryStageRepertoireIsEscapable` pin the zero-delay result and every stage.
**No combination and no pulse sequence announced a region its walking target could not
leave at zero delay.**

The tight rows are rituals. A burial pulse fires 13 ticks after it is shown, and the longest
escape walk inside burial regions was also 13 ticks. An edict pulse fires 15 ticks after it is
shown against a longest walk of 14. The prototype study admitted 1.2 s as the minimum for a
two-block sector or landing transition; the 700 ms burial pulse with its 2.0-block band is under
that minimum, and the delay sweep below shows the consequence.

## Delayed and lost snapshots

Natural fights, readers. The Vargr fights take no hit at any delay up to 800 ms, nor under
either loss pattern. The Draugr fights:

| Condition | Solo, rusty | Solo, iron | Four, iron |
| --- | --- | --- | --- |
| 100 or 250 ms; 1-in-2 or 4-in-20 loss | clean | clean | clean |
| 250 ms and 4-in-20 loss | 1 hit (burial) | clean | clean |
| 400 ms | 1 hit (burial) | 1 hit (burial) | clean |
| 600 ms | 3 hits (2 burial, 1 toll) | 2 hits (burial, toll) | clean |
| 800 ms | 5 hits (2 burial, 3 tolls); 2 before perception | 2 hits (burial, toll); 1 before perception | clean |

Coverage fights, evaders: escaped over threatened windows, then every blow the move landed.
A blow can strike a player who stood outside the region when it was announced and entered it
later, so the blows can exceed the threatened windows that were not escaped.

| Move | 250 ms | 400 ms | 4-in-20 loss |
| --- | ---: | ---: | ---: |
| Vargr prisoner claws | 331 / 332, 1 blow | 293 / 296, 4 blows | 374 / 374, 0 |
| Vargr bite, charge, leap, jaws | all, 0 | all, 0 | all, 0 |
| Draugr sentence, spear | all, 0 | all, 0 | all, 0 |
| Draugr three tolls | all, 0 | 670 / 670, 1 blow | all, 0 |
| Draugr burial | 300 / 302, 2 blows | **83 / 301, 218 blows** | 299 / 300, 1 blow |
| Draugr edict | 75 / 79, 4 blows | 85 / 87, 2 blows | 77 / 78, 1 blow |
| Draugr requiem | 69 / 70, 1 blow | 64 / 70, 6 blows | 70 / 71, 1 blow |

Beyond 250 ms, the burial pulse is what fails: at 400 ms it strikes 218 of 301 threatened
windows. Past the length of a pulse, a region can strike before it is perceived at all.

## Punish windows

Recovery lengths the server actually paid, in ticks, across every measured fight:

| Move | Recoveries paid |
| --- | --- |
| Vargr bite and tear | 8 between bites (887), 36 after the second (841) |
| Vargr prisoner claws | 28 single (612), 8 between paired blows (288), 36 after the pair (286) |
| Vargr collar charge | 50 after an impact (143), 36 otherwise (2) |
| Vargr predator leap | 32 (222) |
| Vargr bonebreaker jaws | 50 (373) |
| Draugr king's sentence | 36 (980) |
| Draugr three tolls | 8 between tolls (1,502), 44 after the third (795) |
| Draugr sepulchre spear | 32 (345) |
| Draugr burial | 40 (380) |
| Draugr edict | 36 (343) |
| Draugr requiem | 40 (162) |

Nearly every charge against a ranged evader ended in the longer impact recovery: the arena
walls and monoliths stop the lane before its end at these positions.

## Contact distance at engagement

The farthest a struck stander's nearest damage sample lay from the boss's centre, at the
2.0-block melee gap, against the visible strike measured in
[#1028](vargr-choreography-1028.md) and [#1034](draugr-choreography-1034.md):

| Move | Struck at | Visible strike | Announced boundary |
| --- | ---: | ---: | ---: |
| Vargr bite and tear | 2.955 | 0.885 / 0.893 | 3.0 |
| Vargr prisoner claws | 2.727 | 0.978 | 3.4 |
| Vargr bonebreaker jaws | 2.727 | 0.834 | 3.6 |
| Draugr king's sentence | 2.493 | 1.520 | 5.0 |
| Draugr three tolls | 2.493 | 1.567 / 1.560 / 1.740 | 3.8 |

These are distances at the 2.0-block gap the scripted melee players hold; a player at the edge
of `SwordReach` stands farther out. At that gap the damage reached between 0.75 blocks (the
third toll's thrust) and 2.07 blocks (the Vargr bite) past the visible strike.

## Findings handed on

1. **Kill times** are 5 or more times shorter than the design's targets in every configuration.
   Recorded here and delivered by #1099, not by any #1037 part.
2. **Burial pulses** have no slack at zero delay and fail at 400 ms. Edict and requiem pulses
   degrade from 250 ms. Part 2.
3. **Contact distance**: blows land 0.75–2.07 blocks past the visible strike at melee gap.
   Parts 2 and 3.

## Limits

- Scripted policies bound what a person can do; they are not a playtest by people. The escape is
  a straight walk: no jump, sprint, shield, mount or route planning.
- Party members start evenly spread around the boss; a crowded or stacked party is not measured.
- No player ever interrupted a requiem: evaders never swing, and natural fights end before
  stage 3 schedules one. The interrupt opening is unmeasured.
- 20 Hz only; the prototype study's 60 Hz escape figures are not repeated here.
- Level 1, full hunger, no food or healing during a fight.
- Coverage fights begin at a stage's health threshold, pulled by hand, and run 300 s in the
  matrix and 90 s in the always-run test.
- Reduced visual effects cannot be tested: the client has no such setting, and it is blocked on
  #1093. Muted audio and presentation are client surfaces and belong to parts 3 and 4.

## Validation

`gofmt`, `go vet`, golangci-lint (0 issues), `go build`, the 386 and arm builds and
`go test ./...` passed, as did the automation shell suite and the DeepSeek Python tests. No
schema, client or gameplay code changed.
