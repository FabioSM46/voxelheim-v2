# First dungeon: what a healer costs the route (#1370)

[#1361](dungeon-healer-1361.md#repeat-after-the-bot-repair-1368) measured a party of three
with one healer at 26.0 minutes and a party of five with one at 23.8, against the 17–23
minute band. This record says why, gives the role-aware estimate #1370 asked for, and
records the owner's decision. **No balance value changes.** None of #1361's evidence is
edited or relabelled.

## The cause

A healer holds a wooden sceptre in the hand a blade would be in.

| | Iron blade | Sceptre orb |
| --- | ---: | ---: |
| Damage to a creature | 40 (`IronSwordDamage`) | 8 (`OrbDamage`) |
| Energy per swing or launch | 25 (`AttackEnergyCost`) | 25 |
| Cooldown | shorter than the refill | 0.75 s, shorter than the refill |
| Pace once the reserve is spent | one every 2 s | one every 2 s |

Both weapons are bound by the same energy refill, so they land at the same pace, and a
sceptre is **a fifth of a blade**. The dungeon still counts its holder as a whole member:
a boss's health and a pack's size read who is inside the instance and never what they hold
(`server/internal/game/boss_scale.go`). A party of three with one healer therefore brings
three members' health to every fight and 2.2 members' blades, and every kill takes 3 / 2.2
as long. At five it is 5 / 4.2.

## The assumption, against the measurement

#1361 ran each party size twice on one build, once with blades only and once with the
last member holding a sceptre. The bots fight the same way in both, so each pair gives a
slowdown that owes nothing to how a bot differs from a person.

| Fight | Blades | One healer | Measured slowdown | Members over blades | Difference |
| --- | ---: | ---: | ---: | ---: | ---: |
| Guardian, three | 206.5 s | 284.7 s | ×1.379 | ×1.364 | −1.1% |
| King, three | 331.0 s | 469.2 s | ×1.418 | ×1.364 | −3.8% |
| Guardian, five | 205.7 s | 248.2 s | ×1.207 | ×1.190 | −1.3% |
| King, five | 343.4 s | 404.7 s | ×1.179 | ×1.190 | +1.0% |

`TestTheHealerSlowdownIsTheOneMeasured` holds the estimate to these within five percent.
The estimate's healer sends every launch to the creature; the measured healers sent 33 of
508 and 17 of 456 to an ally, which is the direction three of the four residuals lean.
Four fights from one run per composition are what there is: this validates the mechanism,
not a confidence interval.

## The estimate

The all-blade route estimate (`dungeon_route_estimate_test.go`) with one substitution:
every kill takes the members over the blades as long. It remains a clean clear — nobody is
struck — so it is the fastest a healer party can be, as the reference is for blades. The
walk does not move; the siege moves less than the fights, because most of its length is
the breathers between waves.

| Party | Blades only | One healer | Against 23 min |
| --- | ---: | ---: | --- |
| Three | 20.0 min | 23.9 min | 0.9 min over |
| Four | 20.0 min | 22.7 min | inside |
| Five | 20.0 min | 22.0 min | inside |

The same at all four rotations the reference test walks, to a tenth of a minute.
`TestOneHealerCostsTheRouteThis` pins the three numbers, so a change to a blade, an orb, a
boss row or a wave that moves them is seen moving them.

## The measured runs, re-read

The bot's human estimate replaces a run's two boss fights with the iron readers' kills. It
printed "unavailable" for a healer party because the readers were all blades. It now
replaces them with the readers' kills at the party's blades:

| Run | Raw clock | Boss fights, measured | Readers at the party's blades | Estimate |
| --- | ---: | ---: | ---: | ---: |
| Three, one healer | 26.0 min | 753.9 s | 724.6 s | 25.5 min |
| Five, one healer | 23.8 min | 652.9 s | 630.1 s | 23.4 min |

These are computed from #1361's one-decimal clocks, not from a new run. Both stay above
23 minutes, and further above the clean estimate than the fights explain: the healer runs
also spent 36 and 18 seconds more in the sand hall and 16 and 18 more in the upper halls,
and the all-blade controls themselves ran 1.4 and 1.8 minutes over the clean 20.0. An
assisted bot's clock is not a clean clear, with or without a healer.

## Decision

Decided by the repository owner on 2026-09-29, from the numbers above:

- **No balance value changes.** Not the orb, not the boss rows, not the tier, not the waves.
- **The 17–23 minute band is the iron-blade reference's.** It is what a party of three to
  five blades takes, and `TestTheRouteTakesSeventeenToTwentyThreeMinutes` keeps holding the
  balance to it. A party that trades a blade for a healer is not held to it.
- **The trade is time for health**, and it is now a recorded number instead of an
  unexplained overrun: two to four minutes of a clean clear for one healer.

Two alternatives were put to the owner and not taken. Raising `OrbDamage` to 16 would bring
three with a healer inside the band, and would change the sceptre everywhere, the open
world included. Sizing a boss for a fraction of a member when that member holds a sceptre
would leave the item alone and break the rule that a boss reads levels and never
equipment (#1099).

## What changed

- `server/internal/game/dungeon_route_healer_estimate_test.go` — the role-aware estimate,
  its validation against #1361 and the pinned route times.
- `server/internal/game/sceptre_share.go` — `OrbBladeShare`, the one share both the
  estimate and the bot's report read, so the number printed is the number validated. It
  decides nothing in the simulation.
- `server/internal/game/dungeon_route_estimate_test.go` — the siege and the blow timing take
  the party's blades; with none traded they are what they were, which
  `TestAPartyOfBladesIsTheReferenceEstimate` checks term for term.
- `server/cmd/voxelheim-descentbot/report.go` — a healer party's report carries the
  estimate, the blades it was taken at, and the statement that the band is the reference's.
  With more than one healer it marks the estimate as extrapolated.
- `server/internal/game/dungeon_balance.go` — the comment on the band says whose it is.

No live run was repeated for this record. Two healers in one party are estimated by the
same arithmetic and were never measured; nothing here asserts a number for them.
