#!/usr/bin/env bash
# =============================================================================
# apt-install-with-fallback.sh — install Ubuntu packages on a hosted runner through
# whichever Ubuntu archive mirror is answering, or fail fast naming every host tried.
#
#   bash scripts/apt-install-with-fallback.sh <package>...
#
# Every workflow that installs the client's system dependencies calls this with the
# same arguments (ci.yml, integration.yml, client-cache.yml), and
# scripts/test/apt-install-fallback.test.sh pins both that and the behaviour below.
#
# ── Why a script and not two apt-get lines (#1107, #1113) ────────────────────
# Every Ubuntu source on a hosted runner resolves through the mirrorlist
# /etc/apt/apt-mirrors.txt, and on 2026-09-11 its hosts failed in three different ways
# in one day: http://archive.ubuntu.com stopped answering, then https to the same host
# answered some connections and not others. That last shape is the one nothing
# before this could survive, and it is a property of addresses, not of hosts:
# archive.ubuntu.com resolves to nine addresses, and probed one by one from outside
# Azure the same day one refused to connect and three stalled mid-body. Each new
# connection is a coin toss.
#
# The previous repair (#1108) rewrote the mirrorlist and appended kernel.org, and it
# could never fail over in time. APT gives each file `Acquire::Retries` attempts per
# mirror, each waiting out `Acquire::https::Timeout`, and serialises a host's files on
# one queue — so run 34586230437 spent 15 seconds per attempt per index on
# archive.ubuntu.com, listed twice, and the step's five minutes ran out with
# kernel.org answering in 200 ms two lines further down. The "green" run beside it
# (34585690348) indexed from archive.ubuntu.com in milliseconds and then spent
# 2min47s downloading four packages from it: a host that answered once is not a host
# that will answer next.
#
# ── What this does instead ───────────────────────────────────────────────────
#   1. Probe every candidate at once, fetching the whole `dists/<codename>/InRelease`
#      within PROBE_SECONDS. The whole body and not a status code: a stalled transfer
#      answers 200 first and times out after.
#   2. Write a mirrorlist of only the hosts that answered, each once, in preference
#      order. None answered: fail now, naming every host and its curl error.
#   3. Run `update` and `install --download-only` under hard deadlines, with
#      `Acquire::Retries=0` and a short network timeout — a failed file moves to the
#      next mirror, which is the retry. An attempt that times out or fails to fetch
#      rotates the list so the next host leads, and the next attempt starts only if
#      the remaining budget can hold a whole one.
#   4. Install from the downloaded archives with `--no-download`, so the part that can
#      hang on a network is over before dpkg runs.
#
# ── What it does not do ──────────────────────────────────────────────────────
# It adds routes and never trust. Sources keep their `Signed-By` keyring, so APT checks
# every InRelease against Ubuntu's archive key whichever mirror served it, and the
# package hashes against that index. No option here relaxes verification, and the test
# pins that none ever appears. A mirror only has to be reachable to be listed; it is
# APT, not the probe, that decides whether what it served is genuine.
#
# Without a mirrorlist (an image that does not use one) there is nothing to reroute:
# the script makes one bounded attempt through the image's own sources.
# =============================================================================

set -uo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <package>..." >&2
  exit 2
fi
PACKAGES=("$@")

MIRRORLIST="${APT_MIRRORLIST:-/etc/apt/apt-mirrors.txt}"

# Preference order. The Azure mirror first because it is the image's own first choice
# and sits in the runners' region; kernel.org next because it answered in milliseconds
# through both runs above; then Ubuntu's two archive hosts, which are the image's other
# two entries; then an independently operated official mirror, so that no single
# operator's outage empties the list.
DEFAULT_CANDIDATES="https://azure.archive.ubuntu.com/ubuntu/
https://mirrors.edge.kernel.org/ubuntu/
https://archive.ubuntu.com/ubuntu/
https://security.ubuntu.com/ubuntu/
https://mirrors.mit.edu/ubuntu/"

# The budgets. The step's timeout-minutes is 5 (300s); PROBE_SECONDS + NETWORK_BUDGET
# leaves more than a minute of it for dpkg and for the failure summary, which is the
# point — a step that is killed prints nothing about why.
PROBE_SECONDS=5
UPDATE_SECONDS=45
DOWNLOAD_SECONDS=30
NETWORK_BUDGET="${APT_FALLBACK_BUDGET:-240}"
APT_OPTIONS=(
  -o Acquire::Retries=0
  -o Acquire::http::Timeout=10
  -o Acquire::https::Timeout=10
)

codename="${APT_CODENAME:-}"
if [ -z "$codename" ] && [ -r /etc/os-release ]; then
  codename=$(. /etc/os-release && printf '%s' "${VERSION_CODENAME:-}")
fi
if [ -z "$codename" ]; then
  echo "::error::cannot read the Ubuntu codename from /etc/os-release; nothing to probe" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

host_of() {
  local rest="${1#*://}"
  printf '%s' "${rest%%/*}"
}

# One reason per line, "<host>\t<reason>", in the order things happened.
reasons="$work/reasons"
: > "$reasons"
blame() {
  printf '%s\t%s\n' "$1" "$2" >> "$reasons"
}

fail_summary() {
  echo "::error::could not install ${PACKAGES[*]}: $1" >&2
  local host
  for host in "${hosts[@]}"; do
    local why
    why=$(awk -F '\t' -v h="$host" '$1 == h { print $2 }' "$reasons" | paste -sd ';' - | sed 's/;/; /g')
    echo "::error::  ${host}: ${why:-answered the probe and never led an attempt}" >&2
  done
  exit 1
}

# ── candidates, deduplicated, https only by construction of the list above ───
candidates=()
hosts=()
while IFS= read -r url; do
  [ -n "$url" ] || continue
  case "$url" in */) ;; *) url="$url/" ;; esac
  dup=false
  for seen in "${candidates[@]}"; do
    [ "$seen" = "$url" ] && dup=true
  done
  $dup && continue
  candidates+=("$url")
  hosts+=("$(host_of "$url")")
done < <(printf '%s\n' "${APT_MIRROR_CANDIDATES:-$DEFAULT_CANDIDATES}" | tr ' ' '\n')

# ── attempt: update, then download, under hard deadlines ─────────────────────
# Returns 0 when both phases fetched everything, 1 otherwise, having blamed the host
# that led the list for this attempt. Blame is attributed to the lead because it is the
# host APT asks first for every file; the log above the summary is the full record.
attempt() {
  local lead="$1" label="$2"
  local log="$work/apt.log" rc

  sudo timeout --kill-after=10 "$UPDATE_SECONDS" apt-get "${APT_OPTIONS[@]}" update 2>&1 | tee "$log"
  rc=${PIPESTATUS[0]}
  if [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ]; then
    blame "$lead" "${label}: apt-get update did not finish within ${UPDATE_SECONDS}s"
    return 1
  fi
  # `apt-get update` exits 0 when an index fails to download, and --error-on=any would
  # also fail on the image's third-party sources, which this step does not need. So the
  # failures that count are the mirrorlist's own.
  if [ "$rc" -ne 0 ] || grep -Eq '^(E: |W: Failed to fetch mirror\+file:)' "$log"; then
    blame "$lead" "${label}: apt-get update failed (exit ${rc}): $(grep -E '^(E|W): ' "$log" | head -1)"
    return 1
  fi

  sudo timeout --kill-after=10 "$DOWNLOAD_SECONDS" apt-get "${APT_OPTIONS[@]}" \
    install -y --no-install-recommends --download-only "${PACKAGES[@]}" 2>&1 | tee "$log"
  rc=${PIPESTATUS[0]}
  if [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ]; then
    blame "$lead" "${label}: downloading the packages did not finish within ${DOWNLOAD_SECONDS}s"
    return 1
  fi
  if [ "$rc" -ne 0 ]; then
    blame "$lead" "${label}: downloading the packages failed (exit ${rc}): $(grep -E '^E: ' "$log" | head -1)"
    return 1
  fi
  return 0
}

install_downloaded() {
  sudo apt-get "${APT_OPTIONS[@]}" install -y --no-install-recommends --no-download "${PACKAGES[@]}"
}

# ── no mirrorlist: one bounded attempt through the image's own sources ───────
if [ ! -f "$MIRRORLIST" ]; then
  echo "::warning::${MIRRORLIST} does not exist; APT's sources cannot be rerouted, making one bounded attempt"
  hosts=("image sources")
  if attempt "image sources" "attempt 1/1"; then
    install_downloaded
    exit $?
  fi
  fail_summary "the only attempt failed"
fi

# ── 1. probe every candidate at once ─────────────────────────────────────────
echo "Probing ${#candidates[@]} Ubuntu mirrors for dists/${codename}/InRelease (${PROBE_SECONDS}s each):"
for i in "${!candidates[@]}"; do
  (
    if err=$(curl -fsS --max-time "$PROBE_SECONDS" -o /dev/null \
               "${candidates[$i]}dists/${codename}/InRelease" 2>&1); then
      : > "$work/probe.$i.ok"
    else
      printf '%s' "${err:-curl failed without a message}" | head -1 > "$work/probe.$i.err"
    fi
  ) &
done
wait

responders=()
for i in "${!candidates[@]}"; do
  if [ -f "$work/probe.$i.ok" ]; then
    echo "  answered  ${candidates[$i]}"
    responders+=("${candidates[$i]}")
  else
    why=$(cat "$work/probe.$i.err" 2>/dev/null)
    echo "  no answer ${candidates[$i]} — ${why}"
    blame "${hosts[$i]}" "probe: ${why}"
  fi
done

if [ "${#responders[@]}" -eq 0 ]; then
  fail_summary "no Ubuntu mirror answered the probe"
fi

# ── 2 and 3. attempts, each led by the next host that answered ───────────────
count=${#responders[@]}
for ((a = 0; a < count; a++)); do
  remaining=$((NETWORK_BUDGET - SECONDS))
  if [ "$remaining" -lt $((UPDATE_SECONDS + DOWNLOAD_SECONDS)) ]; then
    fail_summary "${remaining}s of the ${NETWORK_BUDGET}s network budget left, too little for another attempt"
  fi

  order=("${responders[@]:a}" "${responders[@]:0:a}")
  {
    for p in "${!order[@]}"; do
      printf '%s\tpriority:%d\n' "${order[$p]}" "$((p + 1))"
    done
  } | sudo tee "$MIRRORLIST" > /dev/null

  lead=$(host_of "${order[0]}")
  label="attempt $((a + 1))/${count}"
  echo "::group::${label}, led by ${lead}"
  cat "$MIRRORLIST"
  if attempt "$lead" "$label"; then
    echo "::endgroup::"
    install_downloaded
    exit $?
  fi
  echo "::endgroup::"
  echo "${label} failed: $(tail -1 "$reasons" | cut -f2-)"
done

fail_summary "every mirror that answered the probe failed an attempt"
