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

First, the configuration a local run needs and this repository cannot ship:

```bash
cp .env.example .env    # then fill in what you have; `.env` is git-ignored
. ./.env
```

`.env.example` says what each variable is and what leaving it empty costs. Nothing loads that
file for you — no code here reads a `.env`, and sourcing it is the whole mechanism — and none
of the three commands below needs a value typed into it.

**The one line works because each assignment in that file carries an `export`.** Without one,
sourcing sets a *shell* variable, which a child process does not inherit: `go run` would see
nothing and the file would look loaded while doing nothing. `set -a; . ./.env; set +a` is the
other way to get there — it exports everything assigned between the two — and it still works
on a file that exports itself.

**It lives in the shell, not in the project.** Sourcing it in one terminal does nothing for
another, so the two server commands below either share a shell or each source it first. A
server started from a terminal that never did is a server with an empty environment, which is
a supported state and looks exactly like having forgotten.

```bash
# terminal 1 — the account service (listens on 127.0.0.1:7778, over TLS)
cd server && go run ./cmd/voxelheim-auth \
  -auth-dir "${XDG_DATA_HOME:-$HOME/.local/share}/voxelheim-auth"
# ... voxelheim-auth listening addr=127.0.0.1:7778 certificate_sha256=<64 hex characters>
#                                                  ^^^^^^^^^^^^^^^^^^ copy this

# terminal 2 — the game server
cd server && go run ./cmd/voxelheimd \
  -world-name midgard -account-service https://127.0.0.1:7778 \
  -account-service-fingerprint <that number>

# terminal 3 — the client
cd client && cargo run --release -- \
  --account-service https://127.0.0.1:7778 --account-service-fingerprint <that number> \
  --server 127.0.0.1:7777 --world midgard
```

**The fingerprint is the one thing you have to copy, and it comes out of terminal 1.** The
account service prints it at every start, as `certificate_sha256=…` in the line that also names
the address it bound. It is a hash of the certificate that service hands to everyone who
connects, so it is not a secret — it goes in a chat message or a wiki page beside the address.

**Both consumers refuse without it, and neither will discover one.** `-account-service` and
`--account-service-fingerprint` are required together on their respective commands; there is no
`--insecure`, no trust on first use and no plaintext form of this hop. The reason is the whole
point: the account service is the root of the trust chain — a game server reads its signing key
from it and a client reads the server list from it, and both of those are worth nothing unless
the connection that carried them reached the right machine. First contact is exactly when a
substitution happens, so a number the software discovered would be a number an attacker could
choose.

**It survives restarts and changes when the directory does.** The certificate lives in
`-auth-dir` as `server-cert.pem`, generated on first start and read back afterwards, so the
number is stable until somebody deletes the file or points the service at a new directory. If it
changes, both consumers refuse and say so, naming what they expected and what they were shown —
which is the same refusal a substituted service would produce, because nothing on either side can
tell the two apart.

**`-auth-dir` is deliberately outside the checkout above and in persistent per-user data.**
`$XDG_DATA_HOME/voxelheim-auth` is used when that variable is set, otherwise the command falls
back to `$HOME/.local/share/voxelheim-auth`. Both survive a reboot and routine temporary-file
cleanup. The directory holds the provider-to-account mapping as well as the Ed25519 signing key
and TLS certificate, so losing it makes existing characters unreachable even when the world
itself survives.

The flag's own default (`auth`) resolves against the working directory — so running that command
from `server/` puts a private key inside this repository, one `git add -A` away from a public
commit. `.gitignore` covers the default and the two key file names for that reason, and the path
above keeps the data somewhere that mistake cannot reach.

If an earlier run still has `/tmp/voxelheim-auth`, stop the service and move that whole directory
to the persistent path before using the command above. Moving the whole directory preserves the
accounts, signing key and certificate together. If temporary cleanup has already deleted it, the
random account ids it held cannot be reconstructed from the surviving characters; starting with
an empty directory does not recover them.

**The client needs all four of those flags on this path, and none of them has a default that
would do.** A server admits a player on a signed ticket and nothing else, so a client with no
account service to sign in against is refused whatever address it dials — which is what
`cargo run --release` on its own does, and it says so rather than pretending. `--server` names an
address that is in no list, and `--world` names the world to ask a ticket for, because a ticket
names exactly one world and nothing about an address says which world is running there.

**The Discord client id is in `.env` because signing in is what produces a ticket**, and a
Discord application is something you register rather than something this repository can ship.
`VOXELHEIM_DISCORD_CLIENT_ID` is what the account service reads; `-discord-client-id` takes the
same value and giving it in both is refused rather than resolved by precedence. Left empty in
both, the account service still starts and still publishes its key — the game server comes up
fine — and its two sign-in routes refuse every request, so the login screen appears with nothing
behind it. `server/cmd/voxelheim-auth` documents what to register and where.

**It is not a secret, and the file is not hiding it.** A public OAuth client's id is public by
construction: PKCE stands in for a client secret, the account service holds the verifier, and
there is no client secret anywhere to keep. What `.env` buys is that an account identifier stays
off a command line — this repository is public, and a command carrying one is a command nobody
can paste into an issue, a pull request or a CI log. The registration key beside it in that file
*is* a credential, and is the reason there is no flag that takes one.

**`-world-name` is required and has no default**, and the refusal it produces is the point: a
ticket names one world and is useless at any other, so a server that does not know which world it
is cannot tell its own players' tickets from somebody else's. An empty name resolves to a world id
of zero, which the verifier refuses outright — the server fails at startup rather than starting and
admitting everyone. Lowercase letters, digits and hyphens.

**Exactly one of `-account-service` and `-ticket-key` is required, never both.** The first fetches
the key over the pinned connection above; the second takes it as hex, which is what
`voxelheim-auth` prints at startup — as `public_key=…`, the *other* number in that log, not the
fingerprint — and what `GET /v1/ticket-key` publishes. Use `-ticket-key` when you would rather not
keep the account service running:

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
  -world-name midgard -account-service https://127.0.0.1:7778 \
  -account-service-fingerprint <sha256> -log-level debug -log-format json
cd client && RUST_LOG=info,voxelheim_client=debug cargo run --release -- \
  --account-service https://127.0.0.1:7778 --account-service-fingerprint <sha256> \
  --server 127.0.0.1:7777 --world midgard
```

### Two things that surprise people once each

**The world is on disk by default.** `-world-dir` defaults to
`world-v<WorldgenVersion>`, resolved against the working directory, so a build whose generator is
version 12 writes `server/world-v12/` when run by the command above. It is git-ignored. Restarts of
that build reuse the directory — edits, player records, clock, exploration, structures, markers
and the server's TLS key all survive. `-seed` regenerates the terrain rather than reading it: the
directory holds only what players changed. A deliberate generator bump selects a fresh default
directory automatically; the previous version is left untouched. An explicit `-world-dir` remains
exact, including `-world-dir ""` for an ephemeral world.

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

**Who goes in is chosen after the server answers, not before.** The hello is answered with this
account's characters on that world, and the client puts them on a screen: pick one, or make another
while there is room — a name, and a face built from stated palettes with a live preview of it. The
world arrives only after the server's welcome, which carries the spawn of the character that was
actually picked. Whether a name may be worn is the server's answer and never this client's guess,
so `already taken` and `not acceptable` arrive in the server's own words.

`--name` skips that screen, and it is the same sentence a hello used to carry: it asks for the
character wearing that name and has one created under it when this account holds none. That is what
`voxelheimd` itself did with a display name before V7 moved the choice onto the wire, and it is
what lets an unattended run — `scripts/interop-check.sh` — reach a world. The server decides either
way: a name it refuses is refused with `--name` too. The character played on each server is
remembered under `$XDG_DATA_HOME/voxelheim/characters/<address>` and preselected next time, so the
common case is one keypress — with one exception the wire forces: **a creation is not remembered**,
because `ServerWelcome` names an entity and no character, so a client that has just made one cannot
know the id the server minted for it. The launch after that lists it like any other and selecting
it once is what teaches the file.

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
   DeepSeek (`deepseek-v4-flash`, high reasoning) reviews the diff (one automatic round)
4. **Labeling** — `pr-labeler.yml` applies `READY TO MERGE` only when the frozen
   acceptance rule holds; `/process-pr <n>` force-cycles feedback when you want it now
5. **Merge** — the AI merges ready PRs autonomously into non-main bases; `develop → main`
   promotions remain human-only and re-run the complete matrix

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
| jq    | any recent                      | pipeline scripts, `scripts/test/` |

`jq` is a hard requirement of `scripts/gh-automation.sh` and of the helper test suite, not a
convenience. GitHub's `ubuntu-latest` image ships it, so CI never noticed it was undeclared —
`pr-status` simply printed `[FAIL] ? unresolved review threads (must be 0)` on a workstation
without it and exited 0. Every standalone `jq` call in that script is redirected with
`2>/dev/null`, because a working jq can still be handed an unparseable payload and the
fail-closed sentinel is what must answer that; the same redirection swallowed
`jq: command not found`. `require_jq` now runs before the first API call of every subcommand
that needs the binary and says so in one line instead. Note that gh's built-in `--jq` is a
different thing, evaluated inside gh, and needs nothing installed.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Redistributed copies and derivative
works must preserve the attribution in [`NOTICE`](NOTICE) as required by the license.
