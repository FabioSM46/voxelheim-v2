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

**Three processes now, and the order matters for the first two.** A player is admitted on a
signed ticket rather than a token this server minted, so the game server has to hold the account
service's public key before it can let anybody in — and it reads that key once, at startup.

```bash
# terminal 1 — the account service (listens on 127.0.0.1:7778)
cd server && go run ./cmd/voxelheim-auth -auth-dir /tmp/voxelheim-auth \
  -discord-client-id <your Discord application's client id>

# terminal 2 — the game server
cd server && go run ./cmd/voxelheimd \
  -world-name midgard -account-service http://127.0.0.1:7778

# terminal 3 — the client
cd client && cargo run --release -- \
  --account-service http://127.0.0.1:7778 --server 127.0.0.1:7777 --world midgard
```

**`-auth-dir` is deliberately outside the checkout above.** It holds the account service's
Ed25519 signing key, and its own default (`auth`) resolves against the working directory — so
running that command from `server/` puts a private key inside this repository, one `git add -A`
away from a public commit. `.gitignore` covers the default and the two key file names for that
reason, and the path here points somewhere a mistake cannot reach.

**The client needs all three of those flags on this path, and none of them has a default that
would do.** A server admits a player on a signed ticket and nothing else, so a client with no
account service to sign in against is refused whatever address it dials — which is what
`cargo run --release` on its own does, and it says so rather than pretending. `--server` names an
address that is in no list, and `--world` names the world to ask a ticket for, because a ticket
names exactly one world and nothing about an address says which world is running there.

**`-discord-client-id` is in the first command because signing in is what produces a ticket**, and
a Discord application is something you register rather than something this repository can ship.
Left out, the account service still starts and still publishes its key — the game server comes up
fine — and its two sign-in routes refuse every request, so the login screen appears with nothing
behind it. `server/cmd/voxelheim-auth` documents what to register and where. There is no client
secret to keep: PKCE stands in for one, and the account service holds the verifier.

**`-world-name` is required and has no default**, and the refusal it produces is the point: a
ticket names one world and is useless at any other, so a server that does not know which world it
is cannot tell its own players' tickets from somebody else's. An empty name resolves to a world id
of zero, which the verifier refuses outright — the server fails at startup rather than starting and
admitting everyone. Lowercase letters, digits and hyphens.

**Exactly one of `-account-service` and `-ticket-key` is required, never both.** The first fetches
the key; the second takes it as hex, which is what `voxelheim-auth` prints at startup and what
`GET /v1/ticket-key` publishes. Use `-ticket-key` when you would rather not keep the account
service running:

```bash
cd server && go run ./cmd/voxelheimd -world-name midgard -ticket-key <hex from the auth log>
```

Nothing is asked of the account service after that first read. Admitting a player is a signature
check, so the service being down costs nobody a game.

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
cd server && go run ./cmd/voxelheimd \
  -world-name midgard -account-service http://127.0.0.1:7778 \
  -log-level debug -log-format json
cd client && RUST_LOG=info,voxelheim_client=debug cargo run --release -- \
  --account-service http://127.0.0.1:7778 --server 127.0.0.1:7777 --world midgard
```

### Two things that surprise people once each

**The world is on disk by default.** `-world-dir` defaults to `world`, resolved against the
working directory, so the command above writes `server/world/` — edits, player records and the
server's TLS key. It is git-ignored. `-seed` regenerates the terrain rather than reading it: the
same seed is the same world, and the directory holds only what players changed.

**The development path is encrypted and unverified, and it presents a ticket anyway.** `--server`
(and the default `127.0.0.1:7777`) names an address that is in no server list, so nothing states
which certificate to expect there. The session is still encrypted — there is no plaintext path on
either side — but it is unauthenticated, and a client that cannot verify who answered deliberately
presents no stored *identity* and keeps none. A `voxelheimd` run with `-world-dir ""` mints a new
certificate every start, so nothing would have matched anyway.

What it does present is the session ticket, and that is a stated trade rather than an oversight
(#154). A ticket names one world, expires in hours, and is refused at every other world, so what
an address you typed can do with one is bounded and short — and the alternative was that
development could not connect at all, since a hello with no ticket is refused and is meant to be.
A *stored identity* is the credential that would not be bounded, and it is still never shown here.

Because the ticket names an account, **the character comes back**: the server keys a player on the
account rather than on a token this client kept, so the second launch on this path is the same
character as the first. The client is deliberately not told which of the two happened and does not
claim to know — the status line says `signed in`, and the server's log says `returning=true`.

The way a *player* reaches a server is `--account-service`: sign in once, then click a server out
of the list that service answers with. Every row carries the address and the SHA-256 of the
certificate that server presents, so the address is followed if it moves, and a server presenting
anything else is refused before this client sends a byte — with no way past the refusal. Whoever
runs the server reads the number out of its own startup line —
`msg="listening with an encrypted session" certificate_sha256=…` — and registers *that* with the
account service; until the two agree, the client will not connect. There is deliberately no file
on the player's machine to edit, because a file a player can edit is a file an attacker can talk
them into editing.

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
