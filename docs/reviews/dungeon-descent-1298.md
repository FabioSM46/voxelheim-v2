# First dungeon descent: end-to-end run and instance memory (#1298)

#1298 has three parts:

- **Part 1 (#1329):** the bot that plays the whole dungeon against a real server.
- **Part 2 (#1327):** the zone captures.
- **Part 3:** this record, and the measurement of what one live instance costs.

None of the three changes a gameplay rule, balance number, schema or layout.

**#1333 re-ran the descent with a party.** #1332 sized the dungeon for three to five players
and made it impractical solo, so the solo run below measured a premise the owner has since
replaced. The group runs are in [Group runs (#1333)](#group-runs-1333). The solo figures under
[The run](#the-run) are **superseded by the 3–5 rule** and are kept only as the record of
#1298.

## Group runs (#1333)

`server/cmd/voxelheim-descentbot` now plays with `-party N` bots, from 1 to 5. Each bot is its
own account and character on its own TLS session. The leader invites the others with the
client's `PartyRequest`, each one accepts, and each one walks through the open world's portal.
The server's party rule then puts them all in the same instance, and the bot checks that it did.

- **The party moves one part of the route at a time.** Nobody starts a part until every member
  has finished the one before it. A member that is waiting still fights any creature that turns
  on the party.
- **The puzzles belong to the leader.** The leader presses the rune stones, cuts the web curtain
  and runs the twin levers, and the others wait for the doors to open. The timed grille is for
  everyone: each member must get through it, and whoever finds the lever up pulls it.
- **Every member fights.** Each one strikes whatever has turned on any member. Within 10 blocks,
  each one strikes the most wounded creature, so the party focuses its blows.
- **The members read the bosses.** Each member reads the regions the server announces in each
  `EncounterTimeline` as the client receives them. A member inside one walks the bearing that
  leaves every region soonest, and never steps into one on the way in. This is the #1099
  harness's reader, working over the wire. The #1298 bot never stepped aside, and a party like
  that could not win. An earlier party of three with no reading killed the guardian after 27
  deaths and two wipes, then wiped three times in a row at the king, having taken about 900 of its
  19,800 health.
- **Deaths are real.** No member is ever made immortal. A dead member respawns where the server
  puts it, at the furthest checkpoint the party has reached, and walks back into the part where
  it died. A wipe is every member dead at once. The run gives up after three wipes in a row with
  no kill between them (`-max-wipes`).
- **`/teleport` is used only for portal placement and stuck assists.** A stuck assist now
  teleports only to a cell that the stream still shows as open.

```sh
go build -o <output-directory>/voxelheimd ./cmd/voxelheimd
go run ./cmd/voxelheim-descentbot -server <output-directory>/voxelheimd -party 3
```

All three runs used seed 1 at view distance 4, built from this branch on a shared 16-thread
workstation. The parties of 3 and 5 ran at the same time on separate servers. Every member is a
fresh level-1 character carrying the #1099 iron blade and rusty armour.

### Result

| | Party of 3 | Party of 5 |
| --- | ---: | ---: |
| Instance seed | 5306321562182508718 | 5306321562182508716 |
| Result | cleared, portal to portal | cleared, portal to portal |
| Party wall clock, portal to portal | **21.9 min** | **22.1 min** |
| Estimated human clear | **21.7 min, inside 17–23** | **21.3 min, inside 17–23** |
| Guardian, first pull to kill | 209.9 s | 205.6 s |
| King, first pull to kill | 333.6 s | 375.1 s |
| Deaths per member | 2, 2, 3 | 2, 2, 2, 2, 2 |
| Wipes | 2, both in the upper halls | 2, both in the upper halls |
| Blows taken per member | 28, 37, 34 | 33, 29, 29, 28, 39 |
| `/immortal` | 0 | 0 |
| `/teleport` for portal placement | 6 (2 per member) | 10 (2 per member) |
| `/teleport` stuck assists | 2, both at the timed grille | 0 |

| Part | Party of 3 | Deaths | Party of 5 | Deaths |
| --- | ---: | ---: | ---: | ---: |
| Upper halls | 78.5 s | 7 (2 wipes) | 66.7 s | 10 (2 wipes) |
| Rune hall | 11.6 s | 0 | 12.0 s | 0 |
| Guardian | 213.1 s | 0 | 208.8 s | 0 |
| Chasm and pool | 14.8 s | 0 | 10.4 s | 0 |
| Cave waves | 503.7 s | 0 | 504.6 s | 0 |
| Web curtain | 8.9 s | 0 | 9.3 s | 0 |
| Timed grille | 23.1 s | 0 | 16.3 s | 0 |
| Sand hall | 43.2 s | 0 | 42.5 s | 0 |
| Twin levers | 15.1 s | 0 | 15.3 s | 0 |
| King | 345.0 s | 0 | 386.2 s | 0 |
| Return shortcut | 44.5 s | 0 | 44.1 s | 0 |

Each part's time runs from the moment the party sets off into it to the moment the last member
is through it.

**Creatures killed by the party:**

- **Party of 3:** 6 draugr and 6 vargr, which is four packs of three. Also 36 of 36 spiders
  (twelve waves of three), 3 scorpions, the guardian and the king.
- **Party of 5:** 10 draugr and 10 vargr, which is four packs of five. Also 60 of 60 spiders,
  5 scorpions, the guardian and the king.

The creatures seen include the fresh identities each wipe put back in the upper halls. The
party of 3 saw 5 scorpions because all five buried slots are placed before the hall wakes and
trims them to the pack.

The puzzles held for both parties:

- **Rune hall:** the leader read the order off the inscription, and it matched
  `world.InstanceRuneOrder`.
- **Web curtain:** 15 webs cut.
- **Timed grille:** the leader went from lever to checkpoint in 9.2 s and 9.1 s, inside the
  12 s hold.
- **Twin levers:** 7.6 s and 7.5 s, inside the 10 s window.

### The estimate

The estimate keeps the party's own clock and swaps only the two boss fights for the
energy-economy reader kills at that party size. For a party of 3 those are 206.65 s and 324.70 s.
For a party of 5 they are 204.55 s and 324.70 s. These are the figures
`TestTheRouteTakesSeventeenToTwentyThreeMinutes` adds.

The swap barely moves the number. The bots now read the bosses, so their fights came within
seconds of the readers' at three (209.9 s and 333.6 s) and within a minute at five (205.6 s and
375.1 s). The raw clock sits inside the band too: 21.9 and 22.1 minutes.

Two things add to the #1332 estimate's 20.0 minutes:

- **Walking the route as a group.** Each part waits for the slowest member.
- **The two wipes in the upper halls.** Each wipe sends the party back to fight the pack it lost.

### Solo: dies before the king

A solo attempt (`-party 1`, instance seed 5306321562182508727) **did not reach the guardian**.
The first placed pack in the upper halls is sized for three, and it killed the lone iron delver
three times running without losing a creature. The run gave up at that point, as the 3–5 rule
intends:

- 3 deaths and 3 wipes, with nothing killed.
- 24 blows taken.
- 0 `/immortal`.

### Findings

No defect was found that stops a party of three to five, so no issue was filed. Two observations
are reported, not filed:

- **The upper halls are the hardest part for a level-1 party.** Both parties wiped twice there
  and nowhere else. The hall creatures announce no regions for the members to read, and the
  dungeon tier prices their blows for a mid-level member (`dungeon_balance.go`). A level-1 party
  is below that on purpose.
- **Two members of the party of 3 needed a stuck assist in the timed grille's opening.** Both
  were moved one cell to the checkpoint while the grille still stood open in their stream. That
  is the bot's walker, not the grille.

## The run

> **Superseded by the 3–5 rule (#1332, #1333).** This is the #1298 solo run, with `/immortal`
> on through both boss fights. It is kept as the record of #1298. The group runs above replace it
> as the dungeon's acceptance evidence.

`server/cmd/voxelheim-descentbot` starts `voxelheimd` with an ephemeral world and joins one
character over the real TLS transport. It then plays the route with the same messages the client
sends:

- It walks with `PlayerInput`, facing each next cell and holding forward, and jumps where the next
  cell is one course up. The server's integrator decides where that leaves the body.
- It fights with `AttackRequest`, cuts webs with `MineRequest` and pulls levers and presses runes
  with `MechanismUseRequest`.

The bot knows only what the stream sends it. It plans paths only over delivered chunks as the
`BlockUpdate`s have patched them, so a door is open to the bot only once the server has opened it.
It reads the rune order off the lit runes in the delivered inscription, then checks that order
against `world.InstanceRuneOrder`.

Development commands:

- **`/additem`** gives the #1099 reference iron blade and rusty armour before the portal.
- **`/immortal`** is on for both boss fights, because the bot does not evade. It is also on for the
  rest of any part of the route that has already cost three deaths.
- **`/teleport`** places the bot beside the open world's portal before it walks in, and is
  otherwise only a stuck assist. This run used 2 portal placements and no assists.

```sh
go build -o <output-directory>/voxelheimd ./cmd/voxelheimd
go run ./cmd/voxelheim-descentbot -server <output-directory>/voxelheimd
```

### Result

The run was on seed 1 at view distance 4, with instance seed 5306321562182508727. The source was
part 1's branch, on a shared 16-thread workstation. The bot **cleared the whole route, portal to
portal, in 37.3 minutes** of wall clock:

| Part | Wall clock | Deaths | Notes |
| --- | ---: | ---: | --- |
| Upper halls | 66.0 s | 2 | 2 draugr and 2 vargr killed (the solo share, one per group) |
| Rune hall | 15.1 s | 0 | Read `[2 0 3 1]` from lit counts `[2 4 1 3]`, which matches `InstanceRuneOrder` |
| Guardian | 535.5 s | 0 | 532.9 s fight under `/immortal` |
| Chasm and pool | 10.6 s | 0 | Trapdoor, a 30-course fall and the swim to the shore |
| Cave waves | 490.0 s | 0 | All 12 waves: 20 of 20 spiders, the solo share |
| Web curtain | 9.3 s | 0 | 15 of 15 webs cut, one hit each |
| Timed grille | 16.0 s | 0 | Lever to the checkpoint past the grille in 11.3 s of the 12 s |
| Sand hall | 204.1 s | 3 | 2 scorpions killed; the last 90.8 s under `/immortal` |
| Twin levers | 14.3 s | 0 | 7.5 s from the first lever to the second, of the 10 s window |
| King | 824.8 s | 0 | 812.9 s fight under `/immortal` |
| Return shortcut | 41.3 s | 0 | Walked back to the court, then out through the return portal |

- **Deaths:** 5 in total, with no stuck assists.
- **Blows taken:** 70.
- **Time under `/immortal`:** 23.9 minutes, which is both boss fights and the end of the sand hall.
- **Creatures seen:** 9 draugr and 10 vargr, counting the fresh identities that each wipe
  re-placed; 1 guardian, 2 king identities, 20 spiders and 11 scorpion identities.

### Estimated human time: outside the band, filed as #1328 (superseded)

The bot does not evade, so its boss kill times are not a player's. The estimate therefore keeps
the bot's own clock and swaps only its two boss fights for the #1099 solo iron-reader kill times
(222.65 s and 355.45 s). `TestTheRouteTakesSeventeenToTwentyThreeMinutes` adds the same times.
The result is **24.5 minutes, outside the 17–23 minute band**.

The boss fights are where the estimate breaks down. The bot swings every time the server's energy
reserve allows, so no player can land blows faster. Even so it needs:

- **the guardian:** 532.9 s, against the 222.65 s the estimate assumes;
- **the king:** 812.9 s, against the estimate's 355.45 s.

Attacks cost 25 energy from a reserve that refills at 12.5 a second, so a sustained swing rate is
0.5 a second rather than the cooldown's 1.67. The #1099 kill times predate that economy.
`encounter_playtest_test.go` records the same shift for the starter kit. The route estimate
therefore undercounts both bosses. Without either boss, everything else took the bot 14.9
minutes. So even at the bot's never-evading rate, a solo iron clear takes 37 minutes. This is
reported, not fixed: balance is out of scope here.

## Instance memory

`TestLiveInstanceMemory` in `server/internal/game/instance_memory_test.go` is opt-in and asserts
nothing. It creates eight instances through the production `InstanceManager`, then makes every chunk
of each instance's envelope resident through that instance's own cache. That is the most a party
can ever make it hold. It uses only APIs the pre-feature tree already had, so the same file was run
unchanged on `c687993`, the develop commit before the first descent change.

```sh
VOXELHEIM_INSTANCE_MEMORY=1 go test ./internal/game -run '^TestLiveInstanceMemory$' -count=1 -v
```

| Per live instance | Before the descent (`c687993`) | With the descent |
| --- | ---: | ---: |
| Envelope chunks, shell and halo | 72 | 120 |
| Chunks holding any block | 8 | 24 |
| Created, nothing loaded: live heap | 0.01 MiB | 0.04 MiB |
| Whole envelope resident: live heap | 4.61 MiB | 7.86 MiB |
| Whole envelope resident: resident set | 1.65–1.96 MiB | 11.60–11.82 MiB |

The pre-feature column covers three runs and the descent column two.

- **Live heap is the figure to compare.** It is measured after two collections, and it repeats
  exactly across runs. A resident chunk costs its 64 KiB of voxels, so heap grows with the
  envelope: +48 chunks, +3.25 MiB, or +71% per instance.
- **The resident-set column is a process-wide delta, not a per-instance cost.** It depends on
  whether the allocator could reuse pages it already held. That explains why it is below the heap
  delta before the change and above it after.
- **A live server confirms the order of size.** The bot run's `voxelheimd` grew from 94.8 MiB to
  194.5 MiB on entering one instance. That delta also holds the open-world chunks streamed around
  the portal and the instance's streaming buffers, so it is an upper bound and not the instance's
  own cost.

At the default limit of 32 concurrent instances, the whole envelope resident costs about 252 MiB of
chunk heap, up from 148 MiB before the change.
