# First dungeon boss balance (#1099)

[Part 1 of #1037](dungeon-combat-1037.md) measured every first dungeon fight at least five times
shorter than the approved design: 3.9–32.5 s against the Vargr's 3–4 minutes and 5.9–41.7 s
against the Draugr's 5–6. The server had no party scaling. This record is the balance that closes
that gap, measured with the same harness before and after.

## The rule

`server/internal/game/boss_scale.go`. Computed once, when the encounter is created at the pull,
from **the level of every character inside the run at that moment** and from nothing else.

| Input | Read | Not read |
| --- | --- | --- |
| Member levels, 1–30 | yes | equipment, gear score, class, health, hunger, position |

| Output | Rule | Range |
| --- | --- | --- |
| Boss health | species per-member health × members | 1–4 members; a fifth adds nothing |
| Every boss blow | registry blow × mean over members of `maxHealthFor(level) / PlayerMaxHealth` | 100% at level 1 to 245% at level 30 |

Why these two and no multiplier of their own:

- **A level changes a character's maximum health and nothing else.** A blow is the blade's damage
  (rusty 25, iron 40) at `SwordCooldown` at every level. A health share that grew with level would
  make the same party with the same blades fight longer for having levelled, so each member brings
  the same share and the levels decide only how many shares there are.
- **The damage scale is that same ratio.** A boss blow costs a level-30 member the share of their
  health it costs a level-1 member, so no level makes the boss's moves cheaper to ignore.
- **Clamped at both ends.** Levels are clamped into 1–30, an empty party reads as one member at
  level 1, and members are clamped into 1–4. `TestBossScaleForRepresentativeParties` holds a solo
  low and high level, mixed levels, a full party and both clamps.
- **Never recomputed.** A level-up, an armour change, a death, a disconnect or a return during the
  fight changes nothing (`TestBossScaleIsFixedAtThePull`). A wipe replaces the boss, whose next
  pull is a new encounter. Nobody new may enter during boss combat, so the characters inside at
  the pull are the party that fights.
- **Health fits the wire.** Snapshot health is a uint16, and four members times a boss row must fit
  it (`TestBossHealthFitsTheWireAtEveryScale`). This bound decided the Draugr's number.

Stage thresholds, the snapshot's `MaxHealth` and the boss corpse all read the scaled ceiling, so a
stage begins at the same share of the fight for a party of four as for one player. No move timing,
telegraph, recovery, hazard shape, loot, experience, schema or client code changed.

## Calibration

The per-member health on each boss row is the one number the harness chose.

| Boss | Before (any party) | First pass | Iron readers, 1–4, first pass | Chosen |
| --- | ---: | ---: | --- | ---: |
| Vargr guardian | 720 | 8,800 | 194.40, 169.15, 167.95, 172.45 s (mean 176) | **10,500** |
| Draugr king | 1,200 | 16,000 | 339.25, 299.60, 285.70, 278.45 s (mean 301) | **16,383** |

Kill time follows health almost linearly. The Vargr's middle target of 210 s asked for 10,500. The
Draugr's middle target of 330 s asked for about 17,550, and four times that is over the wire's
65,535. 16,383 is the largest per-member health whose four-member total fits, which leaves a full
party a little under five minutes.

Iron is the reference because two of the three verified entry kits carry the iron blade. The
scale cannot read a kit, so the rusty starter blade's longer fights are the spread the rule
accepts rather than a value it was fitted to.

## Reproduction

From `<repo-root>/server`:

```sh
go test ./internal/game -run 'TestBossScale|TestBossHealth|TestAScaled|TestFirstDungeon' -v
VOXELHEIM_PLAYTEST=1 VOXELHEIM_PLAYTEST_DIR=<review-output> \
    go test ./internal/game -run TestFirstDungeonPlaytest -v
```

The matrix writes `dungeon-combat-1037-fights.csv` and `dungeon-combat-1037-moves.csv`, which now
carry each fight's member levels, the boss's scaled maximum health and its damage percentage. It
adds four level configurations per boss: solo at 10 and 30, levels 1, 10, 20 and 30, and four at
30. The before set is the unchanged matrix on `develop` at c5e9b38, run from a detached checkout;
its kill times match [dungeon-corrections-1037.md](dungeon-corrections-1037.md) to the hundredth.
Both sets are deterministic at 20 Hz and are regenerated rather than committed. On an AMD Ryzen 7
3700X, x86_64 Linux, the before matrix took 50.5 s and the after matrix 116.8 s of wall time with
another run alongside it. The always-run `TestFirstDungeon` tests went from 8.4 s to 16.5 s.

## Kill times

Readers at zero delay, level 1, from the pull to the kill. Brackets give the stage changes.

| Boss | Kit | 1 player | 2 players | 3 players | 4 players |
| --- | --- | ---: | ---: | ---: | ---: |
| Vargr | rusty, before | 32.50 | 12.45 | 9.40 | 5.85 |
| Vargr | rusty, after | 343.35 [129.75] | 340.95 [130.10] | 312.00 [126.35] | 358.20 [130.75] |
| Vargr | iron, before | 19.15 | 7.80 | 5.95 | 3.90 |
| Vargr | iron, after | **222.65** [84.25] | **193.90** [83.05] | **196.45** [80.20] | **190.50** [82.65] |
| Draugr | rusty, before | 41.70 | 17.35 | 11.75 | 8.50 |
| Draugr | rusty, after | 562.90 [218.60, 403.35] | 486.70 [159.45, 328.10] | 474.80 [150.30, 317.20] | 457.20 [145.20, 303.65] |
| Draugr | iron, before | 26.05 | 11.50 | 7.40 | 5.90 |
| Draugr | iron, after | **355.45** [136.70, 252.85] | **306.10** [99.50, 208.05] | **290.40** [94.25, 193.70] | **284.95** [89.90, 189.40] |

Leather and iron armour readers kill at identical times, as before: they swing the same blade and
take no damage.

| Boss | Kit | After, spread | In minutes | Target |
| --- | --- | --- | --- | --- |
| Vargr | iron | 190.50–222.65 s, mean 200.9 | 3.2–3.7 | 3–4 min |
| Vargr | rusty | 312.00–358.20 s, mean 338.6 | 5.2–6.0 | — |
| Draugr | iron | 284.95–355.45 s, mean 309.2 | 4.7–5.9 | 5–6 min |
| Draugr | rusty | 457.20–562.90 s, mean 495.4 | 7.6–9.4 | — |

**Every iron fight of 1–4 players is inside the Vargr's target, and the Draugr's is inside except
for parties of three and four, at 4.8 and 4.7 minutes**, which is the wire bound above. A solo
player in the starter kit finishes both fights, in 5.7 and 9.4 minutes, without a hit.

**Level does not move a reader's kill time.** Solo iron readers at levels 1, 10 and 30 kill the
Vargr in 222.65 s and the Draugr in 355.45 s. Four iron readers at levels 1, 10, 20 and 30 and at
30, 30, 30 and 30 kill them in 190.50 s and 284.95 s, the level-1 times to the hundredth. What the
level moves is the blow: see the stander below.

## Punish windows and recovery

Recovery lengths the server paid in zero-delay natural fights, in ticks, are unchanged: Vargr bite
8 and 36, claws 8, 28 and 36, leap 32, jaws 50; Draugr sentence 36, tolls 8 and 44, spear 32,
burial 40, edict 36. The counts grew with the fights, for example 1,245 bite recoveries after the
second bite against 56 before.

Share of the boss's health removed while it recovered, readers at zero delay, before → after:

| Boss | Kit | 1 | 2 | 3 | 4 |
| --- | --- | ---: | ---: | ---: | ---: |
| Vargr | rusty | 94% → 65% | 59% → 63% | 63% → 57% | 42% → 56% |
| Vargr | iron | 89% → 68% | 61% → 56% | 50% → 57% | 50% → 54% |
| Draugr | rusty | 54% → 55% | 42% → 46% | 29% → 45% | 17% → 42% |
| Draugr | iron | 50% → 55% | 33% → 47% | 20% → 43% | 27% → 43% |

Reading a cycle is still rewarded with the longest opening a fight has. The shares converge because
a fight now runs through every stage many times instead of ending inside the first. A solo Vargr
fight was 94% recovery before only because it ended before the boss chose much else.

## Telegraphs precede damage

Across both sets, **no blow at zero delay, at 250 ms, or under 1-in-2, 4-in-20 or 250 ms with
4-in-20 loss was premature in any natural fight, and no health loss was unattributed or struck
before perception.** The stander's longer fights struck it many more times, every blow after its
promise:

| Move | Stander blows, before | after | Premature |
| --- | ---: | ---: | ---: |
| Vargr bite and tear | 166 | 3,214 | 0 |
| Vargr prisoner claws | 45 | 1,403 | 0 |
| Vargr predator leap | 12 | 16 | 0 |
| Vargr bonebreaker jaws | 17 | 101 | 0 |
| Draugr king's sentence | 63 | 728 | 0 |
| Draugr three tolls | 114 | 1,550 | 0 |
| Draugr sepulchre spear | 12 | 16 | 0 |
| Draugr burial pulse | 31 | 228 | 0 |

`TestFirstDungeonDamageNeverPrecedesItsPerceivedAnnouncement`,
`TestFirstDungeonReadersEscapeEveryAnnouncedRegion` (0, 250 and 400 ms, and 4-in-20 loss, solo
rusty and four iron, both bosses killed without a hit),
`TestFirstDungeonEveryStageRepertoireIsEscapable` and
`TestFirstDungeonRitualPulsesSurviveLatency` pass unchanged.

**The coverage fights are identical before and after** — every escaped, threatened and landed count
at 0, 250 and 400 ms and 4-in-20 loss, including the two claw blows at 250 and 400 ms and the one
toll at 400 ms that part 2 already recorded. Evaders never swing, so the scale changes nothing a
move does.

## The stander and the level scale

A solo stander no longer kills either boss in any kit: 35–41 deaths and as many wipes in 900 s,
where a solo iron stander killed the Vargr in 11.70 s before. Parties of three and four iron
standers still kill both bosses, with deaths. In the rusty kit, three and four standers kill the
Draugr but not the Vargr: 106 and 141 deaths in 900 s.

The level sweep is the damage rule working. A solo iron-armoured stander against the Vargr takes
3,600 damage at level 1, 5,220 at level 10 and 8,820 at level 30 — ×1.45 and ×2.45 — and dies 36
times at each, because its maximum health rose by the same ratio. Against the Draugr: 3,500, 5,075
and 8,575, 35 deaths each.

## What longer fights cost under heavy latency

A fight twelve times longer gives a late region twelve times as many chances to land. Natural
fights, readers:

| Condition | Before | After |
| --- | --- | --- |
| 100–500 ms; every loss pattern | clean | clean |
| 600 ms | clean | Vargr solo iron 1 hit (bite); Draugr solo rusty 4, solo iron 1, four iron 2 (tolls) |
| 800 ms | Draugr solo 3 and 2 hits | Vargr solo rusty and iron **not killed** (120 and 128 hits, 24 and 18 deaths); four iron killed with 40 hits, 4 deaths. Draugr solo rusty and iron **not killed** (103 and 110 hits, 17 and 12 deaths); four iron killed with 66 hits, 2 deaths |

Every one of those blows is premature against its promise: at 600 ms and beyond, a region can reach
a player after its shown interval, which part 2 recorded as a limit and this record does not change.
The criterion is 0 ms, 250 ms and burst loss, and those are clean. At 800 ms, a solo player now
loses a fight they used to win narrowly. That is a finding for latency presentation or move
timing, not something this balance should hide by shortening the fight.

## Limits

- The Draugr's party of three and four is 4.8 and 4.7 minutes against a target of 5–6, because a
  larger per-member health does not fit the uint16 snapshot health. Reaching it needs a schema
  change or a per-member rule that is not linear, and neither is in this issue.
- The rusty starter kit runs 1.6–1.8× longer than iron. The scale cannot read equipment, by
  decision.
- Scripted readers are perfect players at zero delay. People swing less, so real fights run longer
  than these numbers.
- The requiem's 150-damage interrupt is unchanged and still unmeasured. Natural fights now reach
  stage 3, but readers do not aim for the interrupt, and a party of four reaches 150 damage much
  sooner than one player. It remains optional: the pulses are escapable without it.
- A character who was disconnected at the pull and returns during the fight is not counted; the
  fight is the one the others pulled.
- 20 Hz, full hunger and scripted policies, as in parts 1 and 2.

## Validation

`gofmt`, `go vet`, golangci-lint (0 issues), `go build`, the 386 and arm builds and `go test ./...`
passed, as did the automation shell suite and the DeepSeek Python tests. No schema or client code
changed.
