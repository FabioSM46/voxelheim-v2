# schemas/ — The Network Contract

FlatBuffers definitions for every byte that crosses a Voxelheim connection. This directory is
the single source of truth for the wire format: the Go server and the Rust client both consume
**generated** bindings, and neither one defines a message of its own.

Read the root `AGENTS.md` first — it is authoritative for the pipeline. This file covers what
is specific to the contract.

## Layout

| File | Holds |
| ---- | ----- |
| `common.fbs` | `ProtocolVersion`, `Vec3`, `ChunkCoord`, `Appearance`, `HairModel` — included by everything else |
| `handshake.fbs` | `ClientHello`, `ServerWelcome`, `ServerReject`, `RejectReason`; from V7 also `ServerCharacterList`, `CharacterSummary`, `SelectCharacterRequest`, `CreateCharacterRequest` |
| `world.fbs` | `ChunkData`, `ChunkUnload`, `ChunkResendRequest` — voxel streaming; `BlockCoord`, `EditAction`, `BlockEditRequest`, `BlockUpdate` — voxel edits |
| `player.fbs` | `PlayerInput`, mining/inventory-move/attack/drop intent, `EntityState`, item drops, `MobState`, `PlayerVitals`, `EntitySnapshot`, `InventoryState`, `PlayerAppearance` |
| `envelope.fbs` | `Payload` union, `Envelope` root type, `file_identifier` |

Every file declares `namespace Voxelheim.Net;` and includes by plain relative path, so each one
compiles on its own. `envelope.fbs` is the only file with a `root_type`: **one frame on the wire
is exactly one `Envelope`**, which gives both sides a single decode entry point and a single
union switch to keep exhaustive.

## Conventions

- **Types** are `PascalCase`, **fields** are `snake_case`.
- **Enum values** follow their domain: version tokens read as identifiers (`Current`), while
  wire-level error codes read as codes (`PROTOCOL_MISMATCH`) because they are logged verbatim
  on both sides and shown to the player as-is.
- **Structs for fixed-size hot data, tables for everything else.** `Vec3`, `ChunkCoord` and
  `EntityState` are structs so that snapshots and coordinates are inlined instead of reached
  through an offset. A struct can never gain, lose, or reorder a field — that is the price of
  inlining, and it is why only genuinely settled shapes are structs.

  **`Appearance` is the case that shows both halves of the rule.** It is fixed-size and would
  inline perfectly, and it is a table anyway: it is not hot — sent once per character rather
  than once per entity per tick — and it is the least settled shape in the contract, because
  beards, faces and worn equipment are all things a later issue adds. Its home is a message of
  its own for the same reason: `EntityState` is inlined into every snapshot, so five static
  colours put there would be paid for at the tick rate for ever. The argument is written beside
  `PlayerAppearance` in `player.fbs`, and both sides pin `EntityState`'s encoded size at 40
  bytes so that a later field cannot be added to it quietly.
- **Bulk voxel data is a flat scalar vector**, never a vector of tables: a table per run would
  cost an offset each, and a terrain chunk holds thousands of runs.
- Document invariants in the schema itself. A decoder facing untrusted input needs to know that
  `ChunkData.runs` must have even length, no zero-length run, and lengths summing to exactly
  `chunk_size^3` — so that requirement lives next to the field, not in a wiki.
- **Every float on the wire carries a finite requirement**, in both directions, stated on the
  table that owns it. It is deliberately separate from any range clamp: `NaN` compares false
  against every bound, so a clamp passes it through untouched, after which it propagates through
  a simulation or a transform and never leaves. Reject non-finite values at the decode boundary
  rather than trying to clamp them.
- **Anything the receiver divides by, allocates from, or indexes with gets an explicit range**,
  even when it arrives from the authoritative server. Trusting the server on gameplay outcomes is
  not the same as trusting it on array bounds — see `ServerWelcome`.
- **Enum-typed fields need a zero member** so that an absent field fails closed. FlatBuffers
  decodes a missing scalar as its zero value, so `ProtocolVersion.Unknown = 0` exists to make a
  version-less `ClientHello` fail the handshake instead of reading as "current".

## The rules that make it a contract

1. **Fields are append-only.** Never reorder a table's fields, never reuse the id of a removed
   one, never change a field's type. Adding a field at the end is backward compatible; anything
   else is a break.
2. **Union members are append-only** for the same reason: the tag is an integer on the wire.
3. **A break is a version bump.** Raise `ProtocolVersion.Current` in `common.fbs`. The
   handshake then rejects mismatched peers with `PROTOCOL_MISMATCH` instead of letting them
   discover the incompatibility as a decode error mid-session.

   **Whether appending a union member is a break depends on which way it travels**, and rule
   2 alone does not say so. "Append-only" buys backward compatibility only as far as the
   receiver is willing to drop a tag it cannot name — which a client is, and a server is not:
   rule 5 below makes an unrecognised payload a protocol error. `ActionRefused` (tag 20) is
   server→client and shipped without a bump, costing an older client one explanation.
   `DropItemRequest` (tag 25) is client→server, so a newer client against an older server
   would handshake cleanly and die on the first frame it sent — the exact mid-session failure
   this rule exists to prevent. Every client→server member this union has gained carries one.
4. **The client never sends authoritative state.** No client→server message may carry a
   position, a health value, an inventory, or an outcome. `PlayerInput` carries intent; the
   server simulates it and its answer is the truth. A message that lets the client state where
   it is would be a cheat vector by construction, however well the server checked it.

   **Asking is not stating**, and `ChunkResendRequest` is the case that draws the line. It
   names a chunk the client has lost and wants again — a request for *data*, where
   `BlockEditRequest` is a request for a *change* and `PlayerInput` is intent. None of the
   three carries an outcome, and this one carries the least of all: the server decides
   whether the session may have that chunk, what the chunk contains, and how often it will
   answer. A message that let the client say what a chunk *holds* would be the violation;
   one that lets it say which chunk it is missing is not.
5. **Direction is a protocol rule, not a type rule.** Both directions share one union. A peer
   that receives a payload it should never receive treats it as a protocol error and closes the
   connection.

## Generated bindings

Bindings are committed, never hand-edited, and regenerated with the flatc release pinned in
`.flatc-version` at the repo root:

Run a consumer recipe only after that workspace has been scaffolded (`server/go.mod` for Go,
`client/Cargo.toml` for Rust). An absent workspace means there is nothing to regenerate; do not
create its `gen/` directory as a placeholder.

**Server (Go)**, from the repo root, output in `server/gen/`:

```bash
flatc --go --go-module-name github.com/FabioSM46/voxelheim-v2/server/gen -o server/gen -I schemas schemas/*.fbs
gofmt -w server/gen
```

**Client (Rust)**, from the repo root, output in `client/src/gen/`:

```bash
flatc --rust --rust-module-root-file -o client/src/gen -I schemas schemas/*.fbs
flatc --rust --rust-module-root-file -o client/src/gen -I schemas schemas/envelope.fbs
(cd client && cargo fmt --all)
```

Every flag and every line above is load-bearing. Each was found by the output failing to build, so
none of it is decoration:

- **`--go-module-name`**: without it the generated Go files import each other as `Voxelheim/Net`,
  which is not a resolvable module path.
- **`--rust-module-root-file`**: without it, cross-schema imports come out as
  `use crate::<other>_generated::*` — a glob that imports the namespace *module* `voxelheim` rather
  than the types inside `voxelheim::net`, so nothing resolves — and no module root is emitted at all.
  The module-root layout uses relative `use super::*` instead, so the tree compiles wherever it is
  mounted.
- **Two Rust invocations, with `envelope.fbs` last**: flatc rewrites `mod.rs` per input file instead
  of accumulating it, so a single pass leaves a root declaring only the modules reachable from
  whichever schema it happened to process last. On this contract that is 5 of 14, silently omitting
  `envelope_generated` and eight others: the files exist, half the API does not. Passing the root
  schema — the one that includes everything — last produces the complete root.
- **`gofmt` / `cargo fmt` are part of generation, not an edit.** flatc's output is not
  formatter-clean, and both CI format gates cover the generated directories. Running a deterministic
  formatter over generated code is still generation; the rule against hand-editing `gen/` is intact.
  Regenerating and reformatting must produce no diff — `check-schemas.sh` now checks that for
  you, which is why the `schemas` CI job installs both toolchains and why a formatter one minor
  apart would report drift that is not there.

Two consumer-side constraints that belong here because they are consequences of the output layout:

- The Rust bindings **cannot be mounted as `mod gen;`** — `gen` is a reserved keyword in edition
  2024. Mount the directory under another name (`#[path = "gen/mod.rs"] mod wire;`). The *directory*
  keeps the `gen/` name regardless: the review bot's exclusions and the never-hand-edit rule both key
  on that path.
- Generated code needs narrow lint allows on both sides (Go's linter skips files carrying the
  standard generated header; Rust's does not). Keep them scoped to the generated module — never
  relax a lint for the whole workspace to accommodate flatc.

Regenerate **both** sides for any change here: `scripts/changed-areas.sh` fans a `schemas/**`
diff out to the `schemas`, `server` and `client` CI jobs precisely because one contract has two
consumers. The review bot excludes generated paths from the diff it reads, so a hand-edit there
is both wrong and unreviewed.

## Validation

```bash
bash scripts/check-schemas.sh
```

That script is the gate — CI's `schemas` job runs exactly it. It compiles every `.fbs` for
**both** consumers into a throwaway directory, because a contract only one side can generate
code for is a broken contract. A missing flatc is an error; a version other than the pin is a
warning, since generated output may differ from CI's.

**It also checks that the committed bindings are the ones this contract produces.** That is a
second phase, and it is newer than the first because for a long time nothing did it: phase one
generates into a throwaway directory, so editing a contract and forgetting to regenerate passed
every gate in the repository. PR #139 changed one comment and CI went green with both consumers'
`RepairRequest` still carrying the removed text — flatc propagates `///` documentation, so the
contract disagreed with its own bindings and nothing could see it. Phase two runs the recipe above
verbatim and asks git whether anything moved, which is the "must produce no diff" sentence executed
rather than trusted. On drift it leaves the regenerated files in the working tree: the regeneration
is the fix, and reverting it to report a failure would make you do the work twice.

**Know what this gate still does not check.** It asks whether flatc can *generate* code for both
consumers and whether `gen/` matches; it never builds the result. Output that generates cleanly and
does not compile passes here — which is exactly how the wrong Rust command survived in this file
until a consumer tried to build it. The build gates in `server/` and `client/` are what catch that,
so a contract change is not verified until both of those have run.
