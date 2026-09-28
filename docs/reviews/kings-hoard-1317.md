# King's hoard acceptance — issue #1317

## Status

The first live acceptance **failed at the post-restart chest check**. The pre-restart
chest, rune, crafting and paid repair assertions passed. Issue #1317 remains open, blocked
by the separately reported production defect [#1353](https://github.com/FabioSM46/voxelheim-v2/issues/1353).
No gameplay fix or relaxed assertion is included in this evidence record.

Infrastructure landed in #1351 and the scenario/workflow in #1352. The real-server run
below exercises that merged source. Ordinary CI validates the harness implementation;
it does not substitute for this live result.

## Remote procedure

Dispatch **Kings hoard acceptance** (`.github/workflows/hoard-acceptance.yml`) from
`develop`, with `source_sha` equal to the full merged commit under review. The workflow
requires that exact checkout and verifies that it is an ancestor of `origin/develop`.
It builds the real Go server and descentbot from that source, then invokes:

```text
voxelheim-descentbot -server <built-server> -party 3 -hoard -timeout 75m -seed 1
```

The job allows 90 minutes, runs only on explicit dispatch, and uses read-only repository
permissions and pinned actions. It is absent from ordinary CI and Integration. The
existing 3-bot descent includes legitimate combat, deaths and normal mechanism requests;
see [the prior descent evidence](dungeon-descent-1298.md).

## Assertions and evidence

Each member opens chest 1 after the guardian, chest 2 after the timed grille, and chest 3
after the king. A second open must return the same personal container, revision, entries
and silver before consumption. Taking all must deliver exact earned item and currency
deltas; a further open must refuse `ChestAlreadyOpened`. Every member separately claims
the king corpse's real IronSword and KingRune and carries its rune out through the return
portal. The exit purse must equal that member's three chest credits.

The leader places its single table fixture through `PlaceStructureRequest`, observes
its consumption and the placed structure, and crafts the RunicSword through `CraftRequest`.
The rune and pack sword are consumed while the equipped bootstrap sword and purse remain
unchanged. Paid repair chooses the cheapest genuinely worn, affordable item across the
party, with stable member/slot tie ordering. It requests full repair at the capital's
real Forge and compares the complete answer: only target durability and the exact silver
debit may change. Price uses the authoritative `RepairSilverPerPoint` constant.

If no item is affordable, a real chest kit may reduce genuine wear before selection is
retried. No artificial wear, extra death, currency grant or durability injection is used.
If no worn affordable target remains, the run fails and retains its observations.

The harness disconnects, checks a clean server persistence flush, and starts a new process
with the same temporary world and identities. Each client selects its existing character;
its full inventory including durability and silver must match. The party reforms and
votes through the normal portal, verifies the same saved run and seed, and walks to all
three chests. The temporary grille is reopened normally; solved doors must remain open.
Each of the nine member/chest visits must observe `ChestOpen` and an exhausted refusal.
The restarted process must also shut down cleanly before success is reported.

The artifact contains `acceptance.json`, source identity, stage and verdict. Its report
includes per-member chest rolls, king rewards, completion flags, deaths, portal/stuck
assist counts, controlled phase/await labels, fixture/travel notes and repair deltas.
It binds the exact source SHA, workflow run/attempt URL and invocation. Failure exports
partial observations or an explicit missing/rejected-report status and nonzero exit.
Raw output stays in runner scratch: no raw server logs, storage, keys, tickets, addresses,
process paths or binaries are uploaded. A strict prose filter and publication privacy
scan precede artifact upload. The same structured record is available on failures;
raw error text is deliberately excluded.

## Declared setup limits

- The established descent bootstrap supplies one IronSword, RustyHelm, RustyCuirass and
  RustyGreaves per member before entry. This is combat setup, separately recorded from
  earned king loot. Starting silver, runes, runic swords and tables are checked as zero.
- Exactly one EnchantingTable is granted to the leader after legitimate exit. Workstation
  material gathering is outside this acceptance. Placement still uses ordinary rules.
- The established portal placement and stuck-assist policy remains. After exit, overworld
  travel placement reaches an unwarded table site and the capital forge. These assists
  are recorded; they neither skip dungeon gates nor alter earned progression.
- Rune, silver, the consumed iron input and the crafted runic sword come from real loot
  or crafting. No immortality, injected wear or balance/price changes are introduced.

## Live attempt 1 — failed, 2026-09-28

- Tested source: `f52c73366a9d8e95bdd09e8462333f75d336d8b0`.
- [Workflow run 36462056384, attempt 1](https://github.com/FabioSM46/voxelheim-v2/actions/runs/36462056384/attempts/1),
  created 17:59:42 UTC, completed 18:23:16 UTC with failure. This interval includes setup
  and builds; it is not a dungeon-clear timing measurement.
- Artifact: `kings-hoard-f52c73366a9d8e95bdd09e8462333f75d336d8b0-1`, ID `10989466676`.
- Artifact archive digest reported by GitHub:
  `sha256:426faea7103b5f5dd5d400cfee67a0e4f8c6ffd9fec030e6f131833c9d68b82c`.
- Exact sanitized [acceptance.json](evidence/kings-hoard-1317-f52c733/acceptance.json),
  preserved unchanged with [SHA256SUMS](evidence/kings-hoard-1317-f52c733/SHA256SUMS):
  `173365f35ae712f7dd245dfb3c46cb73072c44d46815cab0744706a4e95033f8`.

The workflow used the invocation and declared fixtures above. The report is complete
(`report_status: reported`) and explicitly failed (`exit_code: 1`, `success: false`).
Raw logs, keys, world storage and process diagnostics are not part of this record.

| Member | Chest 1 | Chest 2 | Chest 3 | Earned silver | King personal loot |
| --- | --- | --- | --- | --- | --- |
| 1 | 10 arrows, 11 silver | 1 SharpeningStone, 14 silver | 1 RustyHelm, 29 silver | 54 | IronSword, 3 bones, KingRune |
| 2 | 12 arrows, 11 silver | 1 SharpeningStone, 19 silver | 1 RustyHelm, 22 silver | 52 | IronSword, 4 bones, KingRune |
| 3 | 8 arrows, 10 silver | 1 SharpeningStone, 13 silver | 1 RustyHelm, 26 silver | 49 | IronSword, 5 bones, KingRune |

Every member has all three `ChestDone` flags true: these flags follow the unchanged
personal reopen, exact loot gain, exhausted `ChestAlreadyOpened` refusal and open-block
assertions. All three `KingDone` and `RuneOut` flags are true. King loot awarded no silver.
Each member died twice; each sent zero immortality commands. Stuck-assist counts were
0, 1 and 1, with four portal placements per member across initial entry and re-entry.
The explicit fixtures and travel assists remain acceptance limits, not earned rewards.

The leader placed and consumed the one table fixture at `[249 62 -148]`, consumed the
earned IronSword in pack slot 4 and KingRune, and received a RunicSword while preserving
the equipped bootstrap blade and purse. At the capital forge it repaired the cheapest
affordable actual wear: slot 0, item 7, durability **64 to 100**, silver **54 to 18**,
an exact **36 silver** debit. The chosen target was an already carried item, not a claim
that the newly crafted sword had worn. No kit was needed and no wear was injected.

After the restart, the leader reached stage `saved chest 1`, awaiting `persisted ChestOpen`.
All nine `Restored` flags remained false. The code reaches this wait only after the original
server's checked shutdown, all existing-character full inventory comparisons, party
reformation, normal portal entry, same saved-run ID/seed checks and the leader's walk to
chest 1. Thus those earlier assertions passed. The artifact does **not** record the actual
block ID read: it proves the expected open block was not accepted by the wait, not that
every chest was observed closed. No post-restart chest-use request or repeat loot claim
was attempted after this failed visual check.

Static source inspection identifies a production restoration defect consistent with the
failure: `persist.RewardStore.OverlaySessions` replaces the stored session with the
journal's record, then copies checkpoint/solved-puzzle/cleared-group progress but omits
`OpenedChests`. The journal does not carry ordinary chest opening progress. The initial
session-only overlay branch also omits the detached-copy treatment for this new list.
The sessions-v3 codec and server mapping do carry the field, so the omission is at the
overlay boundary. This is recorded separately in #1353 with regression criteria.
Potential repeat loot is an inference from the first-open branch, not an observed exploit.

Reproduction is the manual dispatch described above at the recorded SHA. After #1353 is
separately addressed and merged, rerun at the exact new merge SHA and inspect all nine
post-restart flags. The current run is useful partial evidence, **not completed acceptance**.
No local tests, builds, lint or live scenario were executed while preparing this report.
