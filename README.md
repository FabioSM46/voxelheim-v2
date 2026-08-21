# Voxelheim

**A cooperative voxel survival RPG — Minecraft × World of Warcraft, set in the dark Norse
winter of the Fimbulvetr.**

A hostile procedural world of fjords, mountains and forests, scarred with ruins and
authentic Younger Futhark inscriptions. Terraform it, survive it, and take your party
through rune-locked instanced dungeons — with real PvE roles (tank / healer / DPS) decided
by the gear you wear, not a class you picked at level one.

The full design document lives in [`docs/GDD.md`](docs/GDD.md).

## Design Pillars

- **Survival only** — shelter, logistics and preparation matter; guild expeditions on foot
  haul the heavy materials that mounts refuse to carry
- **Soft-classing** — roles emerge from equipment (heavy armor generates aggro; no rigid classes)
- **Anti-zerg death loop** — death costs **−20% durability** on everything, never your
  inventory; repairs need consumable kits in the field or fixed stations back home
- **Rune-keyed instanced dungeons** — open-world portals, instance binding to the party,
  daily/weekly lockouts
- **Ward stones & the Fimbulvetr storm** — guild monoliths protect villages from griefing;
  once a week the storm regenerates every unprotected chunk back to its procedural state
- **Modular Norse building** — prefab foundations, walls and roofs instead of block-stacking

## Architecture

| Piece        | Technology                          | Role                                                              |
| ------------ | ----------------------------------- | ----------------------------------------------------------------- |
| `server/`    | **Go**                              | Authoritative simulation: netcode, massive-parallel chunk generation, world state, combat |
| `client/`    | **Rust + Bevy** (ECS on wgpu)       | High-performance rendering with greedy meshing, input, prediction |
| `schemas/`   | **FlatBuffers**                     | Granite network contracts between client and server               |

Monorepo, hybrid client–server. The server decides everything; the client renders and
predicts. `schemas/` is the single source of truth for the wire format, with generated
bindings committed on both sides.

> **Status**: pre-alpha, pipeline-first. The development infrastructure (CI, AI code
> review, scrum automation) is in place; the `server/`, `client/` and `schemas/`
> workspaces are scaffolded through that pipeline as its first issues.

## Repository Layout

```
voxelheim/
├── server/               # Go backend            (scaffolded via pipeline)
├── client/               # Rust/Bevy client      (scaffolded via pipeline)
├── schemas/              # FlatBuffers contracts (scaffolded via pipeline)
├── docs/                 # GDD, workflow, issue conventions
├── scripts/              # Pipeline helpers + their test suite
├── .github/              # CI, DeepSeek review bot, PR labeler, iteration lifecycle
├── .claude/skills/       # Canonical Claude Code skills
├── .agents/skills/       # Generated Codex skill adapters
└── .opencode/skills/     # Generated OpenCode skill adapters
```

## Running It

Two processes, in either order. The client retries a refused connection, and a server nobody
has joined simulates an empty world quite happily.

```bash
# terminal 1 — the authoritative server
cd server && go run ./cmd/voxelheimd     # listens on 127.0.0.1:7777

# terminal 2 — the client
cd client && cargo run --release         # connects to 127.0.0.1:7777
```

`--release` is not a nicety on the client: a Bevy debug build renders slowly enough to be
mistaken for a bug in the renderer.

### Watching what either one is doing

The server logs through `log/slog`; `-log-level` takes `debug`, `info`, `warn` or `error` and
`-log-format` takes `text` or `json`. The client logs through Bevy's `LogPlugin`, so `RUST_LOG`
selects what reaches the terminal — and it draws three lines in the corner of the window that
need no flag at all: the connection and any refusal the server sent, the streamed world (chunks
held, quads merged, last mesh duration), and where the **server** says the player is, which is
the one number that says movement is working.

```bash
cd server && go run ./cmd/voxelheimd -log-level debug -log-format json
cd client && RUST_LOG=info,voxelheim_client=debug cargo run --release
```

### Two things that surprise people once each

**The world is on disk by default.** `-world-dir` defaults to `world`, resolved against the
working directory, so the command above writes `server/world/` — edits, player records and the
server's TLS key. It is git-ignored. `-seed` regenerates the terrain rather than reading it: the
same seed is the same world, and the directory holds only what players changed.

**A client that will not connect is usually the certificate pin, and it is working correctly.**
The client records the SHA-256 of a server's certificate the first time it connects to an address
and refuses any other one afterwards, because there is no domain name here for web PKI to attest.
Two ordinary development situations trip it, and the refusal names the file both times:

- *A new world directory.* The TLS key lives in the world directory, so a server pointed at a
  fresh one — or run with `-world-dir ""`, which keeps nothing and warns at startup that it
  presents a new certificate every start — answers with a fingerprint the client has not seen.
- *An identity with no pin.* A client that played against an earlier server holds an identity
  token for `127.0.0.1:7777` and, if that server predates pinning, no pin beside it. Presenting a
  stored token to an unverified certificate is exactly what pinning exists to prevent, so the
  first connection is refused rather than the second.

You run the server, so you can read the fingerprint it logs at startup —
`msg="listening with an encrypted session" certificate_sha256=…` — and write that one line into
the pin file the refusal names:

```bash
printf '%s\n' "<the certificate_sha256 from the server log>" \
  > "${XDG_DATA_HOME:-$HOME/.local/share}/voxelheim/identity/127.0.0.1_7777.pin"
```

Deleting the *identity* file beside it instead joins as a new character, which pins safely
because a new character has nothing to present. Deleting the *pin* re-pins whatever answers next,
so it is the right move only when you know why the fingerprint changed. Against a server whose
world directory you keep, the key is stable and none of this comes up twice.

Every flag, and what a saved world and a remembered character actually mean, are in
[`server/AGENTS.md`](server/AGENTS.md) and [`client/AGENTS.md`](client/AGENTS.md) under
"Running it".

## Development Process

Every change — feature, bugfix, refactor — flows through the same rigorous loop:

1. **Issue** — created from a structured template (`.github/ISSUE_TEMPLATE/`), refined
   until an AI agent can implement it without guessing
2. **`dev-issue <n>` skill** — Claude Code (`/dev-issue`), Codex (`$dev-issue`) or
   OpenCode (`/dev-issue`) implements it in an isolated git worktree on a branch from
   `develop`, runs every quality gate locally, and opens the PR
3. **CI + AI review** — GitHub Actions runs the change-aware test matrix (`ci-gate`) and
   DeepSeek (`deepseek-v4-flash`, max reasoning) reviews the diff (one automatic round)
4. **Labeling** — `pr-labeler.yml` applies `READY TO MERGE` only when the frozen
   acceptance rule holds; `/process-pr <n>` force-cycles feedback when you want it now
5. **Merge** — a human merges to `develop`. Always. `develop → main` promotions re-run the
   complete matrix

Read the details:

- [`AGENTS.md`](AGENTS.md) — the authoritative pipeline and convention reference
- [`docs/WORKFLOW.md`](docs/WORKFLOW.md) — roles, ceremonies, labels, commands
- [`docs/ISSUE_CONVENTIONS.md`](docs/ISSUE_CONVENTIONS.md) — how to write issues an AI can implement
- [`.github/branch-protection.md`](.github/branch-protection.md) — branch protection contract

## Contributions and Security

The source is public and forkable, but external users receive no direct write access. Changes are
proposed exclusively through pull requests targeting `develop`; maintainers decide whether to
accept them. A fork remains independent and grants no access to this repository.

Do not include real email addresses, personal data, internal machine paths, credentials or private
infrastructure details in issues, pull requests, reviews or commits. Use reserved example values
and GitHub `noreply` identities only.

Do not report vulnerabilities in a public issue. Follow the private reporting instructions in
[`.github/SECURITY.md`](.github/SECURITY.md). Repository publication and access settings are
documented in [`docs/PUBLIC_REPOSITORY.md`](docs/PUBLIC_REPOSITORY.md).

## Toolchain

| Tool  | Version                         | Used by            |
| ----- | ------------------------------- | ------------------ |
| Go    | pinned in `server/go.mod`       | server             |
| Rust  | stable (rustfmt + clippy)       | client             |
| flatc | pinned in [`.flatc-version`](.flatc-version) | schemas |
| gh    | any recent                      | pipeline scripts   |

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Redistributed copies and derivative
works must preserve the attribution in [`NOTICE`](NOTICE) as required by the license.
