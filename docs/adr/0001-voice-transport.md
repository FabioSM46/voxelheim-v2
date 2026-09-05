# ADR 0001 — Proximity voice rides the existing TLS stream

- **Status**: accepted
- **Date**: 2026-09-04
- **Issue**: #849, the first of the voice iteration (#849–#855)
- **Decides for**: #850 (the contract and the relay), #851 (device and mixer), #852 (end to end)
- **Measured by**: #855, under "Measured" below

This is the first architecture decision record in this repository, so a word on the
form: an ADR here states what was decided, what it was decided *against*, and the
evidence. It is not a design document — the design lives in the issues and in the
code — and it is not updated when the code moves. If the decision is revisited, a
later ADR supersedes this one and says so.

## Decision

**Opus frames are relayed by the game server over the TLS stream `transport.ListenTLS`
already carries.** No second process, no SFU, no second port, no second protocol.
A client sends a `VoiceFrame` up the connection it is already authenticated on, the
server decides who may hear it, and it goes back down each recipient's existing
connection as a `VoiceHeard`.

**The client gains exactly two crates: `cpal` for device I/O and `audiopus` for the
codec.** That takes the dependency list from three to five.

## Context

Voice is the first audio feature this client has ever had, and it arrives with two
costs that are easy to conflate and must not be: a *transport* cost, paid by the
server and the wire, and a *dependency* cost, paid by the client's build. This ADR
spends both, on the record, because `client/AGENTS.md` says a fourth crate needs a
discussion before a commit and this issue is asking for a fourth and a fifth.

What the alternative looks like concretely: a self-hosted SFU — LiveKit or
equivalent — running beside the game server, with the client speaking WebRTC to it
and the game server telling it who may hear whom over a control API.

## The measurement

The claim a relay over TLS has to survive is about *timing*. A chunk payload is
worth the same whenever it lands; a voice frame is worthless late. So the question
was measured rather than argued.

`server/internal/transport/voicerelay_measure_test.go` streams 20 ms frames at 50
per second through `transport.ListenTLS` to a client over a loopback socket, with
loss and delay applied by the harness before the write, and reports frame arrival
jitter against the send schedule plus the longest gap the receiver saw. It is not
a CI step: it is skipped unless `VOICE_RELAY_MEASURE` is set.

```bash
cd server
VOICE_RELAY_MEASURE=1 go test ./internal/transport \
    -run TestVoiceRelayJitterOverTLS -v -timeout 5m
```

### The ladder is run twice, and that is the finding

The issue asked for loss and delay. Applied the obvious way — a lost frame is a
frame the harness never writes — the answer is that a relay over TLS costs almost
nothing:

| loss | sent | received | p50 jitter | p99 jitter | longest stall |
| ---- | ---- | -------- | ---------- | ---------- | ------------- |
| 0%   | 500/500 | 500 | 222 µs | 880 µs | 20.32 ms |
| 2%   | 486/500 | 486 | 218 µs | 886 µs | 59.23 ms |
| 5%   | 471/500 | 471 | 224 µs | 884 µs | 80.29 ms |

**That table is true and it is the wrong question**, because it is the answer a
*datagram* transport would give. TCP does not lose a segment, it retransmits it,
and until the retransmission lands every byte queued behind it is held. The frames
after a lost one are not a few hundred microseconds late; they arrive together, one
retransmission timeout later. Loopback cannot produce that on its own, because
loopback never drops a packet — so the harness models it: a lost frame is written
one RTO late and the single writer delivers everything behind it in the burst that
follows. 200 ms is the RTO used, which is Linux's `TCP_RTO_MIN` and the value a
40 ms path clamps to; it is the *optimistic* end, since a second loss of the same
segment doubles it.

| loss | sent | received | p50 jitter | p99 jitter | longest stall |
| ---- | ---- | -------- | ---------- | ---------- | ------------- |
| 0%   | 500/500 | 500 | 214 µs | 877 µs | 20.30 ms |
| 2%   | 500/500 | 500 | 245 µs | **200.26 ms** | **220.44 ms** |
| 5%   | 500/500 | 500 | 795 µs | **200.40 ms** | **220.46 ms** |

Measured on loopback, `amd64`, Linux 7.0, Go 1.26.6, 500 frames per condition,
seed 1, 40 ms one-way delay. Both tables come from one run of the command above.

### What the numbers say

- **On a clean path the transport is free.** Sub-millisecond p99 jitter and a
  longest gap of 20 ms — which is one frame interval, i.e. no gap at all. Nothing
  about TLS, Go's scheduler or the framing in this package costs anything a
  listener could hear.
- **The moment there is loss, the stream transport is the whole cost.** p99 jitter
  goes from 886 µs to 200 ms — three orders of magnitude — and it does so at 2%
  loss, which is an ordinary domestic connection rather than a bad one. The
  distribution is bimodal: the median frame is still on time to within a
  quarter-millisecond, and roughly one frame in fifty is a fifth of a second late
  along with everything behind it.
- **A 220 ms stall is coverable and it is not free.** The jitter buffer #852
  specifies is 60 ms with an adaptive ceiling of 200 ms. At 2% loss that ceiling is
  reached and slightly exceeded, so speech stays continuous only by running the
  buffer at its maximum, and the price is 200 ms of added mouth-to-ear latency for
  everybody on the connection, not only for the frames that were lost.

That is a cost this game can pay. Voxelheim is cooperative PvE — the GDD's
combat is positional and its social unit is a party sharing a fjord, not a
competitive shooter where 200 ms decides a duel. It is emphatically *not* a cost a
competitive game could pay, and if that ever changes this ADR is what to revisit.

## Measured

The measurement above is about the *transport*: 20 ms frames through
`transport.ListenTLS` on a loopback socket, with no session, no handshake, no
simulation and one listener. It answers "does TLS cost a voice anything", and the
answer was no.

**It cannot answer what the relay costs a running server**, because everything the
relay actually does was outside it — the audible sets, the fan-out under `Sim.mu`,
the per-listener latency lane, and the tick loop all of that competes with.
`server/cmd/voxelheim-voicebot` is the end-to-end measurement, added by #855. It
starts a `voxelheimd`, connects synthetic sessions over TLS, walks each through the
real handshake, places them in clusters with the gated `/teleport` command, and has
a configurable share of them send a 96-byte Opus silence frame fifty times a second.
It is not a CI step and it asserts nothing; it prints numbers, and these are them.

```bash
cd server
go build -o /tmp/voxelheimd ./cmd/voxelheimd

# A hundred players in ten conversations of ten, a third of them speaking.
go run ./cmd/voxelheim-voicebot -server /tmp/voxelheimd \
    -sessions 100 -clusters 10 -speaking 0.3 -duration 30s -server-log-level debug

# A thousand players in one conversation, a tenth of them speaking.
go run ./cmd/voxelheim-voicebot -server /tmp/voxelheimd \
    -sessions 1000 -clusters 1 -cluster-radius 10 -speaking 0.1 -duration 30s
```

Each was run twice: once as written, and once with `-speaking 0` and nothing else
changed. **The control run is what makes the numbers a cost rather than a total** —
a thousand co-located sessions are expensive before anybody says a word, and without
the control this section would credit voice with all of it.

### A hundred players in ten conversations

| | 30% speaking | nobody speaking |
| --- | --- | --- |
| frames sent | 45,000 | 0 |
| deliveries owed | 405,000 | — |
| delivered | **405,000 (100.000%)** | — |
| dropped | **0** | — |
| p50 relay latency | 630 µs | — |
| p99 relay latency | 5.35 ms | — |
| longest | 20.22 ms | — |
| achieved tick rate | **20.00 Hz** | 20.00 Hz |
| "fell behind" warnings | 0 | 0 |
| server CPU | 1.05 cores | 0.75 cores |
| server RSS | 484 MiB | 439 MiB |

### A thousand players in one conversation

| | 10% speaking | nobody speaking |
| --- | --- | --- |
| frames sent | 148,870 | 0 |
| deliveries owed | 148,721,130 | — |
| delivered | **537,857 (0.36%)** | — |
| dropped at the latency lane | **148,183,273 (99.64%)** | — |
| dropped at the limiter, the size cap, the audience | **0, 0, 0** | — |
| p50 relay latency | 12.9 s | — |
| p99 relay latency | 29.1 s | — |
| longest | 30.14 s | — |
| achieved tick rate | **1.52 Hz — 658 ms a tick** | **4.23 Hz — 236 ms a tick** |
| "fell behind" warnings | 40 | 64 |
| server CPU | 2.70 cores | 2.26 cores |
| server RSS | 533 MiB | 561 MiB |

Measured on loopback, `amd64` (AMD Ryzen 7 3700X, 8 cores / 16 threads, 31 GiB),
Linux 7.0, Go 1.26.6, seed 1, tick rate 20 Hz, view distance 3, voice range 24
blocks, 96-byte frames at 50 per second, a 30-second window after a 3-second settle.
The relay latency is measured inside the load generator, from the instant a frame
was written to the instant it came back off a listener's socket, so it carries the
generator's own receive scheduling as well as the server's relay; the generator's
own processor figure is printed beside every result for that reason, and in the
hundred-session run it was 0.58 cores of sixteen — not the bottleneck.

### A correction to two of these numbers

**The function that produced them had an off-by-one, and it is fixed rather than explained
away.** `histogram.quantile` truncated the requested rank and then compared with a strict
`>`, so it skipped the bucket whose cumulative count *equalled* the rank: ninety-nine
samples at 35 µs and one at 3 s reported p99 as three seconds, when 99 of the 100 are at or
below 40 µs. #930's review found it.

What it can reach here is bounded, and the bound is arithmetic rather than a guess. The two
forms differ only where `total × fraction` is a whole number — `trunc(x) + 1` is `ceil(x)`
for every other x — and where they differ the wrong one names the *next* bucket, so an
affected figure is too high and never too low.

| run | latency samples | rank at p50 | rank at p99 | affected |
| --- | --- | --- | --- | --- |
| a hundred players | 405,000 | 202,500 — whole | 400,950 — whole | **both, by at most one bucket** |
| a thousand players | 537,857 | 268,928.5 | 532,478.4 | neither |

So **12.9 s and 29.1 s are unaffected**, by arithmetic rather than by re-measurement — which
is what matters, because that is the run this section already says does not reproduce. And
**630 µs and 5.35 ms are each either exact or one bucket high**: both fall in the histogram's
finest tier, where a bucket is ten microseconds, so the most either can carry is 10 µs. The
claim they support — a p99 comfortably inside one 20 ms frame — survives the whole of that
interval.

They are not recomputed, and the reason is stated rather than glossed: the histogram is
per-session state inside a process that has long exited, so there is nothing left to
recompute from. Re-running does not answer the question either, because a re-run measures a
different run — which is the next paragraph.

The first table on this page is not affected: it comes from
`voicerelay_measure_test.go`'s own `percentile`, a different function that indexes a sorted
slice and does not have this defect.

### What the numbers say

- **At the scale the GDD describes, the relay is free.** A hundred players in ten
  huddles delivered every one of 405,000 owed frames, dropped nothing anywhere, and
  left the tick loop at its full 20 Hz with no "fell behind" warning. Voice's own
  share is the difference between the two columns: **0.30 of a core and 45 MiB for
  13,500 deliveries a second**. p99 at 5.35 ms is comfortably inside one 20 ms
  frame, so a listener's jitter buffer never sees a gap.
- **The drop attribution is arithmetic, and one run checks it against the server.**
  The relay answers nothing on the wire and exports no counter; every refusal it
  makes is a `Debug` line. The size cap and the audience are zero by construction
  (96 bytes against a ceiling of 400; every frame asks for `Everyone`), the limiter
  is predicted by running `game.VoiceBurst` and `game.VoiceRefillPerSecond` over the
  harness's own send instants, and the latency lane is the residual. The
  hundred-session run was made at `-server-log-level debug` for exactly this: the
  harness's prediction and the relay's own count agree at **0 and 0**.
- **At a thousand in one place the tick budget is gone before voice is switched
  on.** That is the finding, and it is why the control run exists. With **nobody
  speaking at all**, a thousand co-located sessions run the simulation at 4.23 Hz —
  a mean `Sim.Step` of 236 ms against a 50 ms budget, **4.7 times over it**. Voice
  makes that worse and it does not make it bad: 236 ms becomes 658 ms.
- **What breaks first is the snapshot fan-out, not the relay.** A thousand players
  inside one view distance means every snapshot carries a thousand entities and
  there are a thousand recipients, twenty times a second. That is the cost the
  control run measures and it has nothing to do with voice.
- **Where voice's own frames go is the latency lane, exactly as designed.** Of
  148,721,130 deliveries owed, 537,857 arrived and the rest were refused by a full
  per-session priority queue. `Player.Voice` chose that: a listener whose lane is
  full loses one frame rather than delaying every other listener's, and at a fan-out
  of 999 on a server missing nineteen ticks in twenty the lane is full essentially
  always. **A 99.64% drop rate here is not a voice defect and must not be read as
  one** — it is what that fan-out looks like on a simulation that has already
  stopped keeping time.
- **Deep in saturation the second run is not reproducible, and that is part of the
  result.** The same command was run three times while this section was being
  prepared and delivered 0.36%, 2.70% and 4.24% of what it owed, at 1.52 Hz, 2.25 Hz
  and 2.97 Hz. The table records one run; the spread is what the number is worth.
- **The first run reproduces, but only in part, and the halves are worth separating.**
  Its delivery and tick figures repeat exactly: a later run returned 45,000 sent,
  405,000 owed, 405,000 delivered, nothing dropped and 20.00 Hz, digit for digit. Its
  *latency* does not. That same run, on a host that happened also to be compiling a
  Rust client at a load average of 7.3, measured p50 1.46 ms and p99 13 ms against the
  630 µs and 5.35 ms above. **What the relay delivers is a property of the server; how
  fast it arrives is a property of the machine that afternoon** — so every latency
  figure on this page belongs to an otherwise idle host, and the delivery and tick
  figures do not need one.
  **The "fell behind" counts are not a severity ranking either** — the control
  logged 64 and the voice run 40, because a slower tick produces fewer of them in
  the same wall-clock window.

### Two follow-ups, named rather than fixed here

#855 says to report a `Sim.Step` overrun and name the follow-up rather than tune
inline, and nothing in this measurement changed a constant.

1. **The tick loop does not survive a thousand co-located players, with or without
   voice.** The control run is the evidence and the snapshot fan-out is the suspect:
   O(players²) entity state per tick, before the relay is asked for anything. It
   needs its own issue and it is not a voice issue.
2. **The audible set is unbounded.** `advanceVoiceSetsLocked` puts every player in
   range into every speaker's set, so one crowd is an O(n²) relay however cheap each
   frame is. A cap — the nearest *k* listeners, say — would bound the fan-out at the
   cost of a rule about who is heard, which is a game-design decision and belongs in
   an issue of its own rather than in a constant somebody changes.

**What this measurement does not cover**, stated so nobody reads it as more than it
is: `Sim.Step`'s own per-tick distribution. The achieved tick rate is read from the
counter `EntitySnapshot` carries, so it is a statement about the *mean* over the
window. A p99 tick time needs an instrument inside the server process, and #855
deliberately did not add one.

## Decision drivers

The measurement says the relay is viable, not that it is best. Four things say it
is the right trade here.

1. **The client dependency budget.** `client/AGENTS.md`, "Toolchain and
   dependencies", allows three crates and asks for a discussion before a fourth.
   An SFU does not cost one crate, it costs a WebRTC stack: `webrtc` pulls in an
   async runtime, a DTLS/SRTP implementation, ICE, SDP and their transitive
   graphs — into a client that deliberately has no async runtime and whose whole
   netcode substrate is `std::net` plus `std::sync::mpsc` on one thread. The relay
   costs two named crates and nine lockfile entries in total (measured below), and
   no runtime.
2. **The server's `go.mod` has one dependency.** The relay half of this adds none:
   forwarding an opaque byte slice to a set of sessions is `internal/session` doing
   what it already does for every other message. An SFU adds a control-plane client
   and the operational surface behind it.
3. **There is no second process in the deployment story.** The server is one binary
   that listens on one port. An SFU is a second process, a second port range
   (UDP, which a relay over the existing connection does not need the operator to
   open), a second set of certificates, a second thing to restart and a second
   thing to be down while the game is up. Nothing in this project's deployment
   story has ever had two moving parts.
4. **The rustls precedent of 2026-08-20.** That decision took encryption *into the
   binary* over documenting a WireGuard or VPN deployment, on the grounds that a
   tunnel protects only when every operator configures one correctly and every
   player joins it. The reasoning transfers exactly: a feature that works only when
   the operator stood up a second service correctly is a feature that silently does
   not work, and its failure mode is discovered by players rather than by a refused
   connection.

An SFU is the better answer to a question this project is not asking — many
speakers, selective forwarding, simulcast, browser clients, scale past one server.
When it is asking that question, the answer is a later ADR, not this one.

## The escalation path, and the threshold

**If the relay is not good enough, the answer is an in-house UDP side channel from
this server, never an external SFU.** A datagram lane for voice beside the TLS
stream keeps the single binary, the single deployment and the single authority; it
costs an operator-visible UDP port and a hand-rolled sequence-and-drop path, which
is the `drop` table above and is measurably cheap. Reaching for an SFU would spend
the dependency and deployment budget this decision exists to protect, to solve a
problem that a hundred lines of `net.UDPConn` solve.

**The threshold is the jitter buffer's ceiling: 200 ms.** #852's buffer targets
60 ms and adapts up to 200 ms. If the p99 stall measured against real players at
their real loss rates requires the buffer to sit at or above that ceiling to keep
speech continuous, the relay has stopped being adequate and the UDP lane is built.
The loopback measurement already puts 2% loss *at* the ceiling, so this is a
tripwire that is expected to be tested rather than a theoretical one — which is
why the threshold is written down now, before anybody is attached to the relay.

The measurement to take then is the same harness against a real path, not this one
against loopback: what loopback cannot tell you is what the loss rate actually is.

## The two crates

### `cpal` — device I/O

Capture and playback of raw PCM, on the platform's own audio API. It is the crate
Bevy's own `rodio` backend sits on, so it is not an exotic choice; it is taken
directly because the client needs the *capture* half and no playback library
exposes one.

Measured on the pinned toolchain (1.97.1) with `cargo tree -e normal`: `cpal` 0.16
brings six transitive packages on `x86_64-unknown-linux-gnu` — `alsa`, `alsa-sys`,
`libc`, `bitflags`, `cfg-if`, `dasp_sample` — so seven lockfile entries with itself,
and no async runtime. `alsa-sys` links the system ALSA library through
`pkg-config`, and `libasound2-dev` and `pkg-config` are already in the `client`
job's apt list in `.github/workflows/ci.yml`, so **`cpal` changes the CI
system-dependency list not at all.**

### Why not `bevy_audio`

Bevy's audio feature is playback-only. It can play a decoded stream and it cannot
open a microphone, so the client needs `cpal` whichever way this goes — and having
taken `cpal` for capture, `bevy_audio` would be a second owner of the *output*
device beside it. Two owners of one device is one too many: the mixing, the bus
gains and the per-source spatialisation of #854 all have to happen in one place or
they cannot be reasoned about, and a mixer that owns the output is that place.
`bevy_audio` would also re-enable `rodio` and the image/decoder graph the feature
list in `client/Cargo.toml` was written to keep out.

### `audiopus` — the libopus binding

**Chosen: `audiopus` 0.3.0-rc.0, over `opus` 0.4.0.** Both are safe wrappers over
the same C library. They differ in how they *get* that library, and that is the
whole of the decision.

CI must link against the system `libopus-dev` through `pkg-config`, not compile
libopus from vendored source with cmake. Compiling it means cmake in the toolchain
of every clean build and of every contributor's machine, minutes of C build on a
cold runner, and a copy of libopus whose version is whatever the crate vendored —
so a security fix to Opus arrives through a crate release rather than through the
distribution's package. Linking the system library means one apt package, no cmake,
and the distribution's patch cadence.

`audiopus_sys` probes `pkg-config` first on Unix/GNU targets and falls back to a
cmake build of its vendored source. `opus` 0.4.0 depends on `opusic-sys`, which
bundles libopus and builds it with cmake *by default*, and whose non-bundled path
searches `$PATH` and `OPUS_LIB_DIR` rather than asking `pkg-config` at all.

**Verified by scratch build, not by reading the README.** Both were built on the
pinned toolchain against a system libopus visible to `pkg-config`, with a `cmake`
earlier on `PATH` that fails on sight, so that any cmake invocation turns the build
red:

```bash
# A cmake that refuses to run, first on PATH.
mkdir -p /tmp/nocmake
printf '#!/bin/sh\necho "cmake was invoked" >&2\nexit 1\n' > /tmp/nocmake/cmake
chmod +x /tmp/nocmake/cmake
export PATH=/tmp/nocmake:$PATH

# libopus-dev present (CI: apt-get install -y libopus-dev pkg-config)
pkg-config --modversion opus     # 1.4

cargo new --lib /tmp/probe-audiopus && cd /tmp/probe-audiopus
echo 'audiopus = "0.3.0-rc.0"' >> Cargo.toml
cargo build                      # succeeds; cmake never runs

cargo new --lib /tmp/probe-opus && cd /tmp/probe-opus
echo 'opus = "0.4.0"' >> Cargo.toml
cargo build                      # fails: cmake was invoked
```

`audiopus` built clean, and its build script said so:

```
cargo:info=Found `Opus` via `pkg_config`.
cargo:rustc-link-search=native=<the libdir opus.pc names>
cargo:rustc-link-lib=opus
```

The link-search path is written as a placeholder because the probe was run without
root: `libopus-dev` was fetched with `apt-get download`, unpacked with `dpkg-deb -x`,
and its `opus.pc` re-prefixed at the unpacked tree, with `PKG_CONFIG_PATH` pointed
there. What that changes is the directory in the line above and nothing else — the
build script's route through `pkg-config` is the same one an installed package
takes, which is what the two probes are comparing.

`opus` 0.4.0 failed on the blocked cmake, with a system libopus sitting right there
in `pkg-config` — which is the measurement, not the README's word for it.

`audiopus` adds two lockfile entries, itself and `audiopus_sys`, and no runtime
dependency beyond them; with `cpal`'s seven that is the nine the drivers above
name. It also carries six *build* dependencies — `cmake`, `cc`, `find-msvc-tools`,
`shlex`, `log` and `pkg-config` — which compile on the host and ship in nothing.
`cmake` is among them as a crate even on the path that never invokes the binary,
which is worth knowing before somebody reads the lockfile and concludes this
decision was not taken.

**The negative control matters as much**, because a build that succeeds proves
nothing about *why*. Re-run with `pkg-config` blinded (`PKG_CONFIG_PATH=""`,
`PKG_CONFIG_LIBDIR` pointed at an empty directory) and `audiopus` falls straight
into the cmake path and fails on the blocked binary — so the successful build above
did take the `pkg-config` route rather than merely happening to work.

**The cost of this choice, stated rather than buried**: `audiopus` 0.3.0-rc.0 is a
release candidate from 2021 and `audiopus_sys` 0.2.2 has not moved in about five
years. That is genuinely worse than `opus` 0.4.0, which shipped in August 2026.
Three things make it the right side of the trade anyway, and one of them is an exit:

- The C API being bound is frozen. `opus_encoder_create`, `opus_encode`,
  `opus_decode` and the `CTL` constants have been stable since Opus 1.0 in 2012.
  A binding to a frozen API does not rot the way a binding to a moving one does.
- The surface #852 uses is four functions and a handful of constants. If the crate
  ever fails to build on a future toolchain, replacing it is a day's work, not a
  migration.
- **The exit is named now**: `libopus_sys` 0.4 (`cijiugechu/libopus_sys`) targets
  Opus 1.5, is maintained, and probes `pkg-config` first with the same fallback
  order. It is a `-sys` crate with no safe wrapper, so taking it means writing the
  thin `unsafe` layer `audiopus` provides. That is the fallback if `audiopus`
  breaks, and it keeps the `pkg-config` property this section is about.

**What #851 must therefore do**, and this is the operational consequence of the
paragraph above: add `libopus-dev` beside `libasound2-dev` and `libudev-dev` in all
four places that list holds — `.github/workflows/ci.yml`, `client-cache.yml`,
`integration.yml`, and `scripts/test/client-ci-budget.test.sh`, which asserts the
package list character for character and will go red until it is updated with them.
A missing `libopus-dev` on the runner does not fail loudly — it falls back to the
cmake build and *succeeds*, slowly, having quietly undone this decision. If #851
wants that to be impossible rather than merely documented, a grep for the build
script's "Found `Opus` via `pkg_config`" line is what would pin it.

## Consequences

- `client/AGENTS.md` now names five dependencies, each with the sentence that
  justifies it, and points here.
- The client's CI system-dependency list gains exactly one package,
  `libopus-dev`, in #851.
- `server/go.mod` stays at one dependency. The relay is `internal/session`
  forwarding bytes.
- The wire gains two messages in #850. Voice is the first payload on this
  connection the server forwards without inspecting, which is what makes the
  audibility rule the only thing standing between a speaker and a listener — and
  why that rule is server-side and tested there.
- A voice frame is personal data. The server forwards it and never writes it to
  disk or to a log, which is stated in the GDD's section 10 and is a constraint on
  every later voice issue.
- If the field measurement crosses the 200 ms threshold above, the next ADR is the
  UDP side channel.

## What was not decided here

HRTF and any binaural processing. #854 does distance attenuation, panning and voxel
occlusion with plain DSP and no new crate; if that proves insufficient, a spatial
audio library is its own ADR with its own dependency argument.
