# First dungeon descent: end-to-end run and instance memory (#1298)

#1298 has three parts:

- **Part 1 (#1329):** the bot that plays the whole dungeon against a real server.
- **Part 2 (#1327):** the zone captures.
- **Part 3:** this record, and the measurement of what one live instance costs.

None of the three changes a gameplay rule, balance number, schema or layout.

## The run

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

### Estimated human time: outside the band, filed as #1328

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
