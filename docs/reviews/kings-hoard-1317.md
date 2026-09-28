# King's hoard acceptance — issue #1317

## Status

The live acceptance is **not yet executed**. This document defines the reproducible
procedure and declared setup limits. Ordinary CI validates the harness implementation;
it is not evidence that the three-member live scenario passed.

Infrastructure landed in #1351. The scenario/workflow must land on `develop` before
manual dispatch. A later evidence PR records the exact successful run and may satisfy
issue #1317; this implementation alone leaves that issue open.

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

## Acceptance record to complete after the live run

Record the source SHA, run URL, artifact digest, per-member rolls/refusals, crafting and
repair observations, restart result and any failed assertions. Product defects belong in
separate issues; harness defects may be remediated here. A green workflow plus inspected
complete evidence is required before claiming acceptance. This file currently makes no
such claim.
