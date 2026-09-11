#!/usr/bin/env bash
# Pin how every client job installs its system dependencies: one script, called
# identically by every workflow that carries the step, and what that script does when
# Ubuntu's mirrors do not answer — executed against stubbed mirrors, not read.
#
# History, because it is the reason each case below exists. The step used to repoint
# the runner's mirrorlist in place and append kernel.org (#1107). On 2026-09-11
# archive.ubuntu.com answered some connections and not others, and APT spent 15 seconds
# per attempt per file on it before reaching kernel.org, so the step's five minutes ran
# out with a working mirror listed (#1113). A repair that only reorders hosts cannot
# fail over in time; the script probes, lists only what answered, bounds every attempt
# and rotates the lead.
#
# The stubs stand in for sudo, timeout, curl and apt-get. A host is "dead" to curl when
# it is listed in FAKE_DEAD_HOSTS; apt-get behaves according to the host that leads the
# mirrorlist it was handed, which is the one thing the script controls.
#
# Run: bash scripts/test/apt-install-fallback.test.sh

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/apt-install-with-fallback.sh"

pass=0
fail=0

ok() { echo "  ok   — $1"; pass=$((pass + 1)); }
bad() { echo "  FAIL — $1"; fail=$((fail + 1)); }

assert_eq() {
  if [ "$2" = "$3" ]; then ok "$1"; else bad "$1: expected '$2', got '$3'"; fi
}
assert_contains() {
  if [[ "$2" == *"$3"* ]]; then ok "$1"; else bad "$1: expected to find '$3' in:"; printf '           %s\n' "$2"; fi
}
assert_not_contains() {
  if [[ "$2" != *"$3"* ]]; then ok "$1"; else bad "$1: did NOT expect '$3' in:"; printf '           %s\n' "$2"; fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# ── 1. The step: one script, the same everywhere, inside its budget ──────────
echo
echo "workflows — every carrier installs the same way"
if python3 - "$ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
workflows = root / ".github/workflows"
STEP = "Install Bevy system dependencies"
REQUIRED = {"ci.yml", "integration.yml", "client-cache.yml"}
INVOCATION = (
    "bash scripts/apt-install-with-fallback.sh "
    "libasound2-dev libudev-dev libopus-dev pkg-config"
)


def exactly_one(pattern, text, label):
    matches = re.findall(pattern, text, flags=re.MULTILINE)
    if len(matches) != 1:
        raise AssertionError(f"expected one {label}, found {len(matches)}")
    return matches[0]


def executed(step):
    return [
        line for line in step.splitlines()
        if line.strip() and not line.strip().startswith("#")
    ]


# Found in the tree rather than listed, so a fourth copy is checked the moment it exists.
steps = {}
for path in sorted(list(workflows.glob("*.yml")) + list(workflows.glob("*.yaml"))):
    text = path.read_text()
    if not re.search(rf"^      - name: {re.escape(STEP)}\s*$", text, re.MULTILINE):
        continue
    steps[path.name] = executed(exactly_one(
        rf"^      - name: {re.escape(STEP)}\s*$\n([\s\S]*?)"
        r"(?=^      - (?:name:|uses:)|^  [A-Za-z_][A-Za-z0-9_-]*:\s*$|\Z)",
        text,
        f"{path.name} dependency step",
    ))
assert REQUIRED <= set(steps), f"every client job must carry the step; carriers={sorted(steps)}"
distinct = {tuple(lines) for lines in steps.values()}
assert len(distinct) == 1, (
    "every workflow must install the client's system dependencies identically; "
    f"got {steps!r}"
)
lines = [line.strip() for line in steps["ci.yml"]]
assert lines == [
    "if: ${{ steps.ws.outputs.present == 'true' }}",
    "working-directory: ${{ github.workspace }}",
    "timeout-minutes: 5",
    "run: |",
    INVOCATION,
], f"the step must be the script call and its guards, nothing else; got {lines!r}"

# The in-place repoint is gone everywhere: the script owns the mirrorlist, and a sed
# left in one workflow would put the host it demoted back at the front.
for path in list(workflows.glob("*.yml")) + list(workflows.glob("*.yaml")):
    text = path.read_text()
    assert "Prefer the canonical Ubuntu archive" not in text, f"{path.name} still repoints"
    assert "apt-mirrors.txt" not in text, f"{path.name} still edits the mirrorlist itself"

# ── the script adds routes, never trust ──────────────────────────────────────
script = (root / "scripts/apt-install-with-fallback.sh").read_text()
code = "\n".join(l for l in script.splitlines() if not l.lstrip().startswith("#"))
for forbidden in (
    "--allow-unauthenticated", "trusted=yes", "AllowInsecureRepositories",
    "AllowDowngradeToInsecureRepositories", "allow-insecure", "AllowUnauthenticated",
    "Verify-Peer", "Check-Valid-Until", "--insecure", " -k ", "signed-by", "Signed-By",
):
    assert forbidden not in code, f"the installer must not touch verification: found {forbidden!r}"
defaults = exactly_one(r'^DEFAULT_CANDIDATES="([^"]*)"', script, "DEFAULT_CANDIDATES")
urls = defaults.split()
assert len(urls) >= 3 and len(urls) == len(set(urls)), f"candidates must be distinct: {urls!r}"
assert all(u.startswith("https://") and u.endswith("/ubuntu/") for u in urls), (
    f"every candidate is an https Ubuntu archive root: {urls!r}"
)
assert len({u.split("/")[2] for u in urls}) == len(urls), "one entry per host"

# ── the budgets fit the step, with room left to say why it failed ────────────
def number(name):
    return int(exactly_one(rf'^{name}=(?:"\$\{{[A-Z_]+:-)?(\d+)', script, name))

probe, update, download, budget = (
    number("PROBE_SECONDS"), number("UPDATE_SECONDS"),
    number("DOWNLOAD_SECONDS"), number("NETWORK_BUDGET"),
)
step_seconds = 5 * 60
assert update + download <= budget, "the budget must hold at least one whole attempt"
assert budget + 60 <= step_seconds, (
    f"network budget {budget}s must leave a minute of the {step_seconds}s step for dpkg "
    "and the failure summary — a step that is killed says nothing about why"
)
# Every attempt starts only if a whole one fits, and SECONDS counts from the start, so
# the worst case is the budget plus one attempt's kill-after grace of 10s per phase.
assert budget + 20 < step_seconds, "the kill-after grace must fit as well"
assert probe * 4 <= update, "a probe must cost far less than the attempt it saves"
for option in ("Acquire::Retries=0", "Acquire::http::Timeout=10", "Acquire::https::Timeout=10"):
    assert option in code, f"the per-file bound {option} is missing"

ci = (workflows / "ci.yml").read_text()
automation = exactly_one(
    r"^  automation:\s*$\n([\s\S]*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:\s*$|\Z)", ci, "automation job"
)
assert automation.count("bash scripts/test/apt-install-fallback.test.sh") == 1, (
    "ci.yml's automation job must execute this test exactly once"
)
print(f"  ok   — {len(steps)} workflows agree; budget {probe}s + {budget}s of {step_seconds}s")
PY
then
  pass=$((pass + 1))
else
  bad "static pins (see the traceback above)"
fi

# ── 2. The stubs ─────────────────────────────────────────────────────────────
BIN="$WORK/bin"
mkdir -p "$BIN"
cat > "$BIN/sudo" <<'STUB'
#!/usr/bin/env bash
exec "$@"
STUB
cat > "$BIN/timeout" <<'STUB'
#!/usr/bin/env bash
echo "timeout $*" >> "$CALLS"
while [[ "${1:-}" == --* ]]; do shift; done
shift
exec "$@"
STUB
cat > "$BIN/curl" <<'STUB'
#!/usr/bin/env bash
url="${*: -1}"
host="${url#https://}"; host="${host%%/*}"
echo "curl $url" >> "$CALLS"
for dead in ${FAKE_DEAD_HOSTS:-}; do
  if [ "$dead" = "$host" ]; then
    echo "curl: (28) Connection timed out after 5002 milliseconds" >&2
    exit 28
  fi
done
exit 0
STUB
cat > "$BIN/apt-get" <<'STUB'
#!/usr/bin/env bash
echo "apt-get $*" >> "$CALLS"
lead=""
if [ -f "$APT_MIRRORLIST" ]; then
  lead=$(head -1 "$APT_MIRRORLIST" | cut -f1)
  lead="${lead#https://}"; lead="${lead%%/*}"
fi
listed() { for h in $1; do [ "$h" = "$lead" ] && return 0; done; return 1; }
# Each apt call spends FAKE_APT_ADVANCE seconds of the script's clock, deterministically.
if [ -n "${FAKE_APT_ADVANCE:-}" ]; then
  echo $(( $(cat "$APT_FALLBACK_CLOCK") + FAKE_APT_ADVANCE )) > "$APT_FALLBACK_CLOCK"
fi
case " $* " in
  *" update "*)
    if listed "${FAKE_UPDATE_STALL:-}"; then exit 124; fi
    if listed "${FAKE_UPDATE_WARN:-}"; then
      echo "W: Failed to fetch mirror+file:/etc/apt/apt-mirrors.txt/dists/noble/InRelease  Unable to connect to ${lead}:443:"
      echo "W: Some index files failed to download. They have been ignored, or old ones used instead."
      exit 0
    fi
    # A third-party source failing is not the mirrorlist failing.
    echo "W: Failed to fetch https://packages.example.invalid/dists/stable/InRelease  404"
    exit 0
    ;;
  *" --download-only "*)
    if listed "${FAKE_DOWNLOAD_FAIL:-}"; then
      echo "E: Failed to fetch https://${lead}/ubuntu/pool/main/o/opus/libopus0.deb  Connection timed out"
      exit 100
    fi
    exit 0
    ;;
  *" --no-download "*) exit "${FAKE_INSTALL_RC:-0}" ;;
esac
exit 0
STUB
chmod +x "$BIN"/*

CALLS="$WORK/calls"
export CALLS

run_installer() {
  : > "$CALLS"
  MIRRORS="$WORK/apt-mirrors.txt"
  if [ "${NO_MIRRORLIST:-}" = 1 ]; then rm -f "$MIRRORS"; else printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' > "$MIRRORS"; fi
  OUT=$(PATH="$BIN:$PATH" APT_MIRRORLIST="$MIRRORS" APT_CODENAME=noble \
        bash "$SCRIPT" libasound2-dev libudev-dev libopus-dev pkg-config 2>&1)
  RC=$?
  LOG=$(cat "$CALLS")
  LIST=$(cat "$MIRRORS" 2>/dev/null)
}

AZ=azure.archive.ubuntu.com
KO=mirrors.edge.kernel.org
AR=archive.ubuntu.com
SE=security.ubuntu.com
MIT=mirrors.mit.edu

# ── 3. Behaviour ─────────────────────────────────────────────────────────────
echo
echo "installer — every mirror answers"
run_installer
assert_eq "installs" "0" "$RC"
assert_eq "every candidate listed once, in preference order" \
  "$(printf 'https://%s/ubuntu/\tpriority:1\nhttps://%s/ubuntu/\tpriority:2\nhttps://%s/ubuntu/\tpriority:3\nhttps://%s/ubuntu/\tpriority:4\nhttps://%s/ubuntu/\tpriority:5' $AZ $KO $AR $SE $MIT)" "$LIST"
assert_contains "probes the whole InRelease of the running codename" "$LOG" "curl https://$KO/ubuntu/dists/noble/InRelease"
assert_contains "update is bounded by a hard deadline" "$LOG" "timeout --kill-after=10 45 apt-get"
assert_contains "failover to the next mirror is the retry" "$LOG" "-o Acquire::Retries=0 -o Acquire::http::Timeout=10 -o Acquire::https::Timeout=10 update"
assert_contains "the download is bounded too" "$LOG" "timeout --kill-after=10 30 apt-get"
assert_contains "and fetches only" "$LOG" "install -y --no-install-recommends --download-only libasound2-dev libudev-dev libopus-dev pkg-config"
assert_contains "dpkg runs with the network already done" "$LOG" "install -y --no-install-recommends --no-download libasound2-dev libudev-dev libopus-dev pkg-config"
assert_eq "one attempt when the first one works" "1" "$(grep -c '^apt-get .* update$' <<<"$LOG")"
assert_not_contains "a third-party index failure does not fail the attempt" "$OUT" "attempt 1/5 failed"
assert_not_contains "no apt call relaxes verification" "$LOG" "allow-unauthenticated"

echo
echo "installer — archive.ubuntu.com and Azure do not answer the probe"
FAKE_DEAD_HOSTS="$AR $AZ" run_installer
assert_eq "installs through what answered" "0" "$RC"
assert_eq "only the hosts that answered are listed" \
  "$(printf 'https://%s/ubuntu/\tpriority:1\nhttps://%s/ubuntu/\tpriority:2\nhttps://%s/ubuntu/\tpriority:3' $KO $SE $MIT)" "$LIST"
assert_contains "the log names a host that did not answer, and why" "$OUT" "no answer https://$AR/ubuntu/ — curl: (28) Connection timed out"

echo
echo "installer — no mirror answers"
FAKE_DEAD_HOSTS="$AZ $KO $AR $SE $MIT" run_installer
assert_eq "fails" "1" "$([ "$RC" -ne 0 ] && echo 1 || echo 0)"
assert_not_contains "without calling apt at all" "$LOG" "apt-get"
assert_contains "saying why" "$OUT" "::error::could not install libasound2-dev libudev-dev libopus-dev pkg-config: no Ubuntu mirror answered the probe"
for h in $AZ $KO $AR $SE $MIT; do
  assert_contains "naming $h and its error" "$OUT" "::error::  $h: probe: curl: (28) Connection timed out"
done
assert_contains "and leaves the image's mirrorlist alone" "$LIST" "azure.archive.ubuntu.com/ubuntu/	priority:1"

echo
echo "installer — the first host answers the probe, then stalls in update"
FAKE_UPDATE_STALL="$AZ" run_installer
assert_eq "installs" "0" "$RC"
assert_eq "two attempts" "2" "$(grep -c '^apt-get .* update$' <<<"$LOG")"
assert_contains "the stall is named" "$OUT" "attempt 1/5 failed: attempt 1/5: apt-get update did not finish within 45s"
assert_eq "the next host leads, and the stalled one moves to the back" \
  "https://$KO/ubuntu/	priority:1" "$(head -1 <<<"$LIST")"
assert_eq "without being dropped" "https://$AZ/ubuntu/	priority:5" "$(tail -1 <<<"$LIST")"

echo
echo "installer — update 'succeeds' with the mirrorlist's indexes missing"
FAKE_UPDATE_WARN="$AZ $KO" run_installer
assert_eq "installs on the third lead" "0" "$RC"
assert_contains "a failed index is a failed attempt, whatever the exit status" "$OUT" "attempt 2/5 failed: attempt 2/5: apt-get update failed (exit 0): W: Failed to fetch mirror+file:"
assert_eq "archive.ubuntu.com leads the attempt that worked" "https://$AR/ubuntu/	priority:1" "$(head -1 <<<"$LIST")"

echo
echo "installer — indexes arrive, packages do not"
FAKE_DOWNLOAD_FAIL="$AZ" run_installer
assert_eq "installs" "0" "$RC"
assert_contains "the download failure is named" "$OUT" "downloading the packages failed (exit 100): E: Failed to fetch"
assert_eq "exactly one dpkg run" "1" "$(grep -c -- '--no-download' <<<"$LOG")"

echo
echo "installer — every host that answered fails its attempt"
FAKE_DEAD_HOSTS="$SE $MIT" FAKE_UPDATE_STALL="$AZ $AR" FAKE_DOWNLOAD_FAIL="$KO" run_installer
assert_eq "fails" "1" "$([ "$RC" -ne 0 ] && echo 1 || echo 0)"
assert_contains "saying so" "$OUT" "every mirror that answered the probe failed an attempt"
assert_contains "naming the probe failures" "$OUT" "::error::  $SE: probe: curl: (28)"
assert_contains "and each attempt's host with its reason" "$OUT" "::error::  $AR: attempt 3/3: apt-get update did not finish within 45s"
assert_contains "including a download failure" "$OUT" "::error::  $KO: attempt 2/3: downloading the packages failed"
assert_not_contains "and never runs dpkg" "$LOG" "--no-download"

echo
echo "installer — the budget cannot hold another attempt"
# Driven by the script's injectable clock rather than by sleeping: the outcome may not
# depend on how long the host takes to start the script and run the probe (#1118 review).
CLOCK="$WORK/clock"
echo 0 > "$CLOCK"
APT_FALLBACK_CLOCK="$CLOCK" FAKE_APT_ADVANCE=100 FAKE_UPDATE_STALL="$AZ $KO $AR $SE $MIT" run_installer
assert_eq "fails" "1" "$([ "$RC" -ne 0 ] && echo 1 || echo 0)"
assert_eq "after the attempts a 240s budget holds, instead of spending the step" "2" "$(grep -c '^apt-get .* update$' <<<"$LOG")"
assert_contains "saying how much budget was left" "$OUT" "40s of the 240s network budget left, too little for another attempt"
assert_contains "naming each host it tried" "$OUT" "::error::  $KO: attempt 2/5: apt-get update did not finish"
assert_contains "and the hosts it never reached" "$OUT" "::error::  $AR: answered the probe and never led an attempt"

echo
echo "installer — exactly one attempt's worth of budget still starts that attempt"
echo 165 > "$CLOCK"
APT_FALLBACK_CLOCK="$CLOCK" FAKE_APT_ADVANCE=100 FAKE_UPDATE_STALL="$AZ $KO $AR $SE $MIT" run_installer
assert_eq "one attempt at 75s remaining" "1" "$(grep -c '^apt-get .* update$' <<<"$LOG")"
assert_contains "and none after it" "$OUT" "-25s of the 240s network budget left"

echo
echo "installer — a failure after the network is not retried"
FAKE_INSTALL_RC=100 run_installer
assert_eq "dpkg's exit status is the step's" "100" "$RC"
assert_eq "and no second attempt fetches anything" "1" "$(grep -c '^apt-get .* update$' <<<"$LOG")"

echo
echo "installer — an image without a mirrorlist"
NO_MIRRORLIST=1 run_installer
assert_eq "installs through the image's sources" "0" "$RC"
assert_not_contains "without probing hosts it cannot route to" "$LOG" "curl"
assert_eq "and creates no mirrorlist" "" "$LIST"
assert_contains "saying it cannot reroute" "$OUT" "cannot be rerouted"

echo
echo "installer — usage"
OUT=$(PATH="$BIN:$PATH" bash "$SCRIPT" 2>&1); RC=$?
assert_eq "no packages is a usage error" "2" "$RC"

echo
echo "── ${pass} passed, ${fail} failed ──"
[ "$fail" -eq 0 ] || exit 1
