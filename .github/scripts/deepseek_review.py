#!/usr/bin/env python3
"""DeepSeek V4 Flash Automated PR Reviewer — review-only, no commits, no file changes.

Ported from the clinic-deck Kimi reviewer; clinic-deck PR numbers in comments refer
to the incidents there that shaped these rules.
"""

import json
import os
import re
import sys
from typing import NamedTuple

from github import Auth, Github, GithubException
from requests.exceptions import RequestException
from openai import APIStatusError, APITimeoutError, AuthenticationError, OpenAI

# DeepSeek V4 documents a 1M-token context and a 384K maximum output, the same for
# flash and pro. The executable guard stays at 384,000: below the provider ceiling
# whether the documentation's K is decimal or binary. Faithful legacy PR 80 replays
# exhausted both 65,536 and 131,072 tokens entirely in reasoning, and 262,144 held
# until MAX_CHARS was raised — a diff five times larger reasons for longer before it
# has a verdict, so the default now *is* the documented maximum rather than a step
# below it. There is no headroom left above this; a review that exhausts it is a
# review that needs a smaller diff, not a larger budget.
DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS = 384_000
DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS = 384_000

# The largest diff, in characters, that is sent to the model. A module constant rather
# than a local so that it can be read by a test and pinned against the documentation.
#
# **This number is measured, and the two before it were not.** 120_000 described the
# model's context window and stopped being true when the model changed; 600_000 described
# the context window of the model that replaced it, and was never the thing that bounds a
# review at all. What bounds it is the **output** budget: the chain of thought is emitted
# into the same DEEPSEEK_MAX_OUTPUT_TOKENS the verdict has to fit in, and at
# DEEPSEEK_REASONING_EFFORT=max the reasoning is what exhausts it. A 124,711-character diff
# reasoned to the last token of a 384,000-token ceiling and had none left to write a
# verdict with, after 31 minutes and a full spend (#167) — and then a 60,863-character one
# did the same in 33 minutes, well inside the cap that run was supposed to be safe under
# (#491).
#
# The arithmetic, from the runs that reached the API:
#
#   *  45,415 chars (PR #501) — verdict, two findings, one of them a real defect
#   *  50,963 chars (PR  #80) — two runs on the same diff: 530,226 reasoning chars and no
#                               verdict against a 131,072-token ceiling, then a verdict in
#                               35,966 completion tokens against 262,144
#   *  60,863 chars (PR #488) — 1,448,213 reasoning chars, finish_reason=length, no verdict
#   *  64,167 chars (PR #168) — passed at 384,000 in 7m38s
#   *  72,350 chars (PR #169) — passed at 384,000
#   * 124,711 chars (PR #164) — 1,481,442 reasoning chars, finish_reason=length, no verdict
#
# 1,481,442 characters emitted for 384,000 tokens is 3.86 characters per token, so the
# whole output budget is about 1,481,000 characters.
#
# **The outcome is not monotonic in diff size, and that is the finding.** 72,350 succeeded
# and 60,863 did not. Size is only a proxy for the binding variable, and the binding
# variable is how hard the model reasons about *that particular* content: #164 reasoned at
# 11.9 characters per character of diff, #488 at 23.8 — twice as hard, on a diff half the
# size. 23.8 against a 1,481,000-character budget puts the fill point at about 62,300, but
# that is an estimate carrying #164's characters-per-token, and the number to anchor on is
# the one #488 measured: it emitted 1,448,213 characters at 60,863 and had nothing left to
# write a verdict with after 33 minutes, so **60,863 is an observed fill point** and the
# 62,300 estimate agrees with it to within 2.4% (#491). Corroboration, not the anchor.
#
# 90,000 was derived from 11.9, which was the only ratio anything had measured against the
# ceiling it was setting a margin under. A cap set from one observation of a quantity that
# varies 2x keeps failing on the hard half of the distribution, and this is the third time
# this number has had to come down.
#
# **45,000 is set from the worst observed ratio rather than the average, because the two
# failure directions are not symmetric.** Too high costs a 33-minute run that produces
# nothing, a failing `review` check and a pull request that cannot merge until somebody
# splits it by hand — and nothing in the log says the size was the problem until you open
# the job. Too low is loud: the diff is truncated, every dropped file is named in the log
# *and* injected into the review as a finding, and the pull request blocks until a human
# acknowledges the gap. One costs half an hour and a manual split; the other costs a
# DEEPSEEK_REVIEW_READ click. Set the number from the tail.
#
# The margin is the one 90,000 already used, taken against the observed point rather than
# the estimated one: 90,000 was 72% of the 124,711 #164 measured, and **45,000 is 74% of
# the 60,863 #488 measured.** That is a point *thinner* than the precedent rather than as
# generous — the precedent's exact 72.2% of 60,863 is 43,900, and a flat 72% is 43,800 —
# and it is stated rather than rounded away, because the gap is inside the noise of a
# ratio with two samples and that is exactly the kind of claim this comment keeps honest.
#
# 45,000 spends about 277,400 tokens reasoning at 23.8 and leaves roughly 106,600 against
# the 384,000 ceiling; at 11.9 it spends about 138,500 and is never close. PR #501 is the corroboration that this is not
# over-tightened: 45,415 characters came back with a verdict and two substantive findings,
# so the safe ceiling is bracketed between a measured success at 45,415 and a measured
# failure at 60,863, and 45,000 sits just under the success.
#
# **It costs review latency, deliberately.** Five of Iteration 29's seven issues already
# needed splitting at 90,000; at 45,000 most changes become two or three pull requests.
# That is the price of a review that answers, and it is the reason this number should not
# be lowered further without a measurement forcing it.
#
# **Raising it again is what needs more samples; lowering it did not.** Two runs measure
# the ratio *at exhaustion* — #164 at 11.9 and #488 at 23.8 — and those two are what the
# cap is set from, because in each of them the 384,000-token ceiling this cap sits under is
# what stopped the reasoning. The rest of the list bounds the ratio without sampling it:
# reasoning_chars is printed only where a run returns no content, so a success reports
# completion_tokens instead, and #80's replay printed 530,226 characters against a
# 131,072-token ceiling it *hit*, which makes its 10.4 a floor rather than a value.
#
# What #80 does measure is worth more than a third point on the curve, because both of its
# runs are the same 50,963-character diff: one reasoned past a 131,072-token ceiling, the
# other finished inside 35,966 completion tokens — about 2.7 characters per character.
# **The ratio varies run to run on identical input, not only between diffs**, which is the
# strongest argument available that a cap set from one observation was never going to hold.
# A third and fourth sample would say whether 23.8 is the tail or the new middle, and
# `measure_only: true` on the workflow's dispatch is how to get them: it replays a real
# diff without posting a review or spending a round. Until then the asymmetry decides the
# direction on its own.
#
# **It is a truncation threshold and not a promise** — a review that still exhausts the
# budget under it is a new measurement, and this number is what has to come down.
#
# The ratio is a property of DEEPSEEK_REASONING_EFFORT and of the model. Change either and
# every number above has to be measured again.
DEEPSEEK_MAX_DIFF_CHARS = 45_000


def _no_verdict_remedy(finish_reason):
    """What an operator should do about a review that produced no verdict.

    **The advice this replaces was impossible to follow.** It said to raise
    DEEPSEEK_MAX_OUTPUT_TOKENS, and the ceiling was already the provider's maximum — so the
    one sentence an operator had to act on named a lever that did not move (#167). Which
    remedies exist depends on where the ceiling actually is, so this asks rather than
    assuming.

    A `length` finish under the diff cap is the interesting case and gets its own sentence:
    the cap is a measured number, and a diff inside it that still exhausts the budget is a
    measurement saying the number is now wrong. That is a fact about the configuration
    rather than about the pull request, and telling somebody to split a PR that is already
    inside the cap would send them to fix the wrong thing.
    """
    if finish_reason != "length":
        return (
            "This is not an output-budget failure — finish_reason is "
            f"{finish_reason!r}. Read the run log before changing any budget."
        )

    if DEEPSEEK_MAX_OUTPUT_TOKENS < DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS:
        return (
            "The model ran out of output budget. Raise DEEPSEEK_MAX_OUTPUT_TOKENS "
            f"(currently {DEEPSEEK_MAX_OUTPUT_TOKENS}) towards the provider limit of "
            f"{DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS}, and re-derive the request/job timeout "
            "budget with it."
        )

    return (
        "The model ran out of output budget and the ceiling is already the provider's "
        f"maximum ({DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS}), so there is no budget to raise. "
        f"Every diff reaching this call is at or under DEEPSEEK_MAX_DIFF_CHARS "
        f"({DEEPSEEK_MAX_DIFF_CHARS:,}) — a larger one was truncated before it — so this is "
        "a measurement saying that cap is now too high. Lower it and its documentation "
        "together, and note what changed: the ratio it was derived from belongs to the "
        "model and to DEEPSEEK_REASONING_EFFORT."
    )


def _read_output_token_budget(environ=None):
    """Read and validate the output ceiling before any API request is made."""
    source = os.environ if environ is None else environ
    raw = source.get(
        "DEEPSEEK_MAX_OUTPUT_TOKENS",
        str(DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS),
    )
    normalized = str(raw).strip()
    if not normalized.isdecimal():
        raise ValueError(
            "DEEPSEEK_MAX_OUTPUT_TOKENS must be a positive base-10 integer; "
            f"got {raw!r}"
        )

    value = int(normalized)
    if value < 1 or value > DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS:
        raise ValueError(
            "DEEPSEEK_MAX_OUTPUT_TOKENS must be between 1 and "
            f"{DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS} (DeepSeek V4 provider limit); "
            f"got {value}"
        )
    return value


# Parsed at process startup, before main constructs either the DeepSeek or GitHub
# clients. A malformed workflow value therefore fails closed without spending an
# API call or publishing a partial review.
DEEPSEEK_MAX_OUTPUT_TOKENS = _read_output_token_budget()

# How long to wait for ONE DeepSeek completion, in seconds. The real budget is this times
# (DEEPSEEK_MAX_RETRIES + 1), and *that* product is what must stay below `timeout-minutes` on the
# job in deepseek-pr-review.yml — currently 2700s x 2 = 90min against a 100min cap.
#
# Why the ordering matters: when the SDK's deadline fires, this script prints a diagnostic and
# exits 1. When the *job* cap fires first, the step is reported as `cancelled` with no output at
# all — and `pr-status-json` counts CANCELLED as a failing check, so the PR sticks at needs-work
# with nothing explaining why (clinic-deck PR #298, killed at a 10-minute cap mid-generation).
#
# Why not the SDK defaults: their retry budget is not coordinated with the Actions job cap. Why
# not zero retries either: that was tried, and it made a transient
# `Connection error` fail the whole job where the SDK would have recovered silently. One retry buys
# back flake resilience — cheap, because connection errors fail fast — while keeping the worst case
# bounded. A retried *timeout* costs the full budget and still fails diagnosably, which is the
# acceptable end of the trade.
DEEPSEEK_REQUEST_TIMEOUT_SECONDS = float(os.environ.get("DEEPSEEK_REQUEST_TIMEOUT_SECONDS", "2700"))
DEEPSEEK_MAX_RETRIES = int(os.environ.get("DEEPSEEK_MAX_RETRIES", "1"))

# Flash at maximum reasoning is the deliberate default: it keeps review depth
# close to Pro while reducing API cost and synchronous Actions runner time.
DEEPSEEK_MODEL = os.environ.get("DEEPSEEK_MODEL", "deepseek-v4-flash")
DEEPSEEK_REASONING_EFFORT = os.environ.get("DEEPSEEK_REASONING_EFFORT", "max")

# Stamped into every Mode A review body; round accounting counts only reviews
# carrying it. GitHub records a standalone review-comment reply as an implicit
# COMMENTED review, so a plain state filter counts thread replies as review
# rounds — clinic-deck PR #260, where 6 "rounds" were really 2 reviews. The
# marker cannot be inferred from the body being non-empty: an inline-only Mode A
# review has an empty body too. Keep in sync with DEEPSEEK_FULL_REVIEW_MARKER in
# scripts/gh-automation.sh.
FULL_REVIEW_MARKER = "<!-- deepseek:full-review -->"

# Marks the "review paused" notice so it is posted at most once per PR.
PAUSED_NOTICE_MARKER = "<!-- deepseek:review-paused -->"

# Marks a review body that carries NO findings — the clean approve and nothing else.
#
# The merge gate reads findings structurally: a DeepSeek review whose body still has
# content after the markers are stripped is holding feedback that no review thread
# counts, and it blocks READY TO MERGE until a human acknowledges it (clinic-deck
# #466). That rule derives from what is in the body rather than from a marker the
# model has to remember, so it also covers reviews this script never stamped —
# including the APPROVE-with-general-comments shape (clinic-deck #478).
#
# The clean approve is the one body that is prose and yet says nothing, so it is the
# one case that needs marking. Keep in sync with DEEPSEEK_NO_FINDINGS_MARKER in
# scripts/gh-automation.sh: an approve stamped here and unrecognised there simply
# blocks the label until someone clicks, which is the safe direction but a pointless
# click.
NO_FINDINGS_MARKER = "<!-- deepseek:no-findings -->"


def main():
    api_key = os.environ.get("DEEPSEEK_API_KEY")
    gh_token = os.environ.get("GITHUB_TOKEN")
    repo_name = os.environ.get("REPO")
    event_name = os.environ.get("EVENT_NAME")

    if not api_key:
        print("ERROR: DEEPSEEK_API_KEY not set")
        sys.exit(1)

    if not gh_token:
        print("ERROR: GITHUB_TOKEN not set")
        sys.exit(1)

    pr_number_str = os.environ.get("PR_NUMBER")
    if not pr_number_str:
        print("ERROR: PR_NUMBER not set")
        sys.exit(1)
    pr_number = int(pr_number_str)

    client = OpenAI(
        api_key=api_key,
        base_url="https://api.deepseek.com",
        timeout=DEEPSEEK_REQUEST_TIMEOUT_SECONDS,
        max_retries=DEEPSEEK_MAX_RETRIES,
    )
    gh = Github(auth=Auth.Token(gh_token))
    repo = gh.get_repo(repo_name)
    pr = repo.get_pull(pr_number)

    bot_username = _resolve_bot_username(gh, gh_token)
    print(f"Running as GitHub user: {bot_username}")

    if event_name == "pull_request":
        mode_full_review(client, repo, pr, bot_username)
    elif event_name == "pull_request_review_comment":
        comment_body = os.environ.get("COMMENT_BODY", "")
        comment_id_str = os.environ.get("COMMENT_ID", "0")
        if not comment_id_str or comment_id_str == "0":
            print("ERROR: COMMENT_ID not set")
            sys.exit(1)
        comment_id = int(comment_id_str)
        comment_author = os.environ.get("COMMENT_AUTHOR", "")
        mode_reply(client, repo, pr, comment_body, comment_id, comment_author, bot_username)
    else:
        print(f"Unknown event: {event_name}, exiting.")
        sys.exit(0)


# ──────────────────────── helpers ────────────────────────


def _sanitize_error(err: str) -> str:
    """Remove potential API key leaks from error strings before logging."""
    err = re.sub(r"Bearer\s+\S+", "Bearer ***REDACTED***", err)
    # Redact common API key patterns: sk-..., and similar prefixes with 20+ base62 chars
    err = re.sub(r"\b[a-z]{2,4}-[A-Za-z0-9_-]{20,}", "***REDACTED***", err)
    return err


def _safe_int(value, default=None):
    """Robust int conversion that handles float strings like '3.0'."""
    try:
        return int(float(value))
    except (ValueError, TypeError):
        return default


def _count_bot_reviews(pr, bot_username):
    """Count the bot's Mode A full reviews — the only thing MAX_ROUNDS caps.

    Matching on FULL_REVIEW_MARKER rather than on state alone keeps thread-reply
    wrappers and the paused notice from spending the round budget.
    """
    try:
        reviews = list(pr.get_reviews())
    except (GithubException, RequestException) as exc:
        # Not 0. A lookup that failed does not mean "no rounds spent" — it means the
        # count is unknown, and the two justify opposite actions: 0 lets another review
        # run, so during an outage the one-round cap could be bypassed indefinitely,
        # each run unable to see the ones before it. Failing here is the same fail-closed
        # rule the frozen rule follows for every count it reads.
        raise RuntimeError(
            "Could not read this pull request's existing reviews, so whether the round "
            f"budget is already spent is unknown. GitHub said: {exc}"
        ) from exc
    return sum(
        1 for r in reviews
        if r.user.login == bot_username
        and r.state == "COMMENTED"
        and FULL_REVIEW_MARKER in (r.body or "")
    )


def _stamp(body):
    """Prefix a review body with the round-accounting marker.

    Renders as nothing on GitHub, so an inline-only review still reads as
    body-less while remaining countable.
    """
    return f"{FULL_REVIEW_MARKER}\n\n{body}" if body else FULL_REVIEW_MARKER


def _post_paused_notice(pr, existing, max_rounds, bot_username):
    """Announce the paused review at most once, as an issue comment.

    Deliberately not a review: a COMMENT review would be counted as a round by
    both counters and would re-post on every subsequent push (clinic-deck #260
    ended up with notices reading "3 times" then "4 times").
    """
    try:
        for c in pr.get_issue_comments():
            if c.user.login == bot_username and PAUSED_NOTICE_MARKER in (c.body or ""):
                print("Paused notice already present — not repeating it.")
                return
        pr.create_issue_comment(
            f"{PAUSED_NOTICE_MARKER}\n"
            f"DeepSeek has reviewed this PR **{existing} "
            f"{'time' if existing == 1 else 'times'}** (limit: {max_rounds}). "
            f"Full review paused — further pushes will not re-review. "
            f"Resolve the existing threads, or force another pass with "
            f"`bash scripts/gh-automation.sh pr-deepseek-force-review {pr.number}`."
        )
        print("✓ Paused notice posted")
    except GithubException as exc:
        print(f"ERROR posting paused notice: {exc}")


def _resolve_bot_username(gh, gh_token):
    """Resolve the account that will post reviews without confusing it with the PR actor."""
    configured = os.environ.get("DEEPSEEK_BOT_USERNAME", "").strip()
    if configured:
        return configured

    # Actions' built-in token usually cannot read /user and posts as github-actions[bot].
    if gh_token.startswith(("ghs_", "ghu_")):
        return "github-actions[bot]"

    try:
        return gh.get_user().login
    except GithubException as exc:
        status = getattr(exc, "status", "unknown")
        print(f"WARNING: Could not resolve GitHub token user (status={status}); assuming github-actions[bot]")
        return "github-actions[bot]"


# Dependency lockfiles, by exact basename. Deliberately not a suffix match: a
# hand-written `.lock` file elsewhere in the tree is not one of these, and the cost
# of guessing wrong is a source file that never gets reviewed.
LOCKFILE_NAMES = frozenset({"Cargo.lock", "go.sum"})


def is_generated_path(filename):
    """
    True for machine-generated artifacts that must never consume the review budget.

    FlatBuffers codegen is committed (both sides vendor the generated bindings so
    builds need no flatc at compile time), so contract PRs carry large generated
    diffs. Reviewing generated output is worthless in both directions: nobody acts
    on a finding about regenerated bindings, and the round budget is one. The
    clinic-deck ancestor of this rule watched generated artifacts crowd 13 real
    source files out of a review's char budget (clinic-deck #399) — same failure mode.

    Convention (documented in AGENTS.md): all generated code lives under a `gen/`
    path segment, and flatc's Rust output additionally carries the `_generated.`
    infix in its filename.

    Dependency lockfiles are the same category by a different mechanism: cargo and
    `go mod` write them, nobody reviews a resolved version graph, and they are
    enormous. On legacy PR 15 `client/Cargo.lock` was 5264 of the 8319 non-generated
    lines — 63% of the diff the model was asked to read, for a file whose only
    reviewable fact (which dependencies exist) lives in the manifest next to it.
    The manifests themselves stay in: a new dependency in `Cargo.toml` or `go.mod`
    is exactly what a reviewer should see.
    """
    slashed = f"/{filename}"
    if "/gen/" in slashed:
        return True
    basename = filename.rsplit("/", 1)[-1]
    return "_generated." in basename or basename in LOCKFILE_NAMES


class Diff(NamedTuple):
    """What the reviewer was actually able to read.

    `text` alone is not enough for a caller to decide anything, and returning only
    `text` is what produced two silent failures: an unreadable diff read as an empty
    one (legacy PR 31), and a truncated diff read as a complete one, so a clean verdict was
    published for a pull request whose entire server half was never seen (legacy PR 32). The
    two extra fields exist so that neither fact can be lost between here and the
    decision that depends on it.
    """

    text: str
    #: Files with a real patch that the model did not fully see, because the budget
    #: ran out before them. Empty for a complete review.
    dropped: list
    #: True when the diff could not be fetched at all. Distinct from an empty diff,
    #: which is a legitimate state — every file excluded as generated, for instance.
    unreadable: bool


def get_diff(pr):
    """Fetch the unified diff for PR files using pr.get_files() (handles forks, binary files, renames)."""
    try:
        files = list(pr.get_files())
    except (GithubException, RequestException) as exc:
        # RequestException as well as GithubException: PyGithub raises the former when
        # urllib3 exhausts its retries, which is what a 503 storm produces. That escaped
        # as a raw traceback until legacy PR 43 — the run failed, which was the right direction,
        # but what a human saw was a stack trace instead of "GitHub is unavailable".
        print(f"ERROR fetching PR files (GitHub API unavailable or refusing): {exc}")
        # Not an empty diff. The caller must be able to tell the difference, because
        # "nothing changed" and "we could not look" justify opposite actions.
        return Diff(text="", dropped=[], unreadable=True)

    parts = []
    binary_count = 0
    excluded = []
    dropped = []
    withheld = []
    for f in files:
        if is_generated_path(f.filename):
            excluded.append((f.filename, getattr(f, "status", "modified"), len(f.patch or "")))
            continue
        if f.patch:
            src = f.previous_filename or f.filename
            parts.append(f"--- a/{src}\n+++ b/{f.filename}\n{f.patch}")
        elif (getattr(f, "additions", 0) or 0) + (getattr(f, "deletions", 0) or 0) > 0:
            # A file with textual changes and no patch is not a binary file: it is a patch
            # the API declined to send. GitHub reports 0/0 lines for a genuine binary, so
            # the two are separable — and until they were, a degraded API produced a diff
            # of three characters for a 636-line pull request, which legacy PR 31's guard could not
            # see because nothing had raised (legacy PR 43).
            #
            # What that cost: the model was handed those three characters, and — with the
            # pull request's own description still in the prompt — improvised two findings
            # complete with symbol names and line numbers for code it had never seen. A
            # reviewer that says nothing is a problem; one that speaks without reading is a
            # different and worse one.
            withheld.append(f.filename)
        else:
            binary_count += 1

    if binary_count:
        print(f"Skipped {binary_count} binary/large file(s) without readable diff")

    if withheld:
        print(f"ERROR: GitHub returned {len(withheld)} file(s) with changes but no patch — "
              "the response is incomplete, so no review can be based on it:")
        for name in withheld:
            print(f"  - {name}")
        return Diff(text="", dropped=[], unreadable=True)

    diff = "\n\n".join(parts)

    # What bounds this is the output budget, not the context window — see
    # DEEPSEEK_MAX_DIFF_CHARS for the measurements and the arithmetic. Truncation reports
    # every dropped file and blocks the pull request, because a review nobody can see the
    # gaps in is worse than none.
    #
    # **This guard is why an over-large pull request is answered rather than crashed
    # into.** It did not fire between roughly 124,000 and 600,000 characters, which was the
    # band where the model runs out of output budget: the run reached the API, spent the
    # whole ceiling reasoning and exited on a missing verdict, with nothing anywhere saying
    # the size was the problem. A cap the model can actually reach is what turns that back
    # into the outcome this code was written for — a partial review, every unread file
    # named, and a human who has to acknowledge the gap before the PR can merge (#32).
    MAX_CHARS = DEEPSEEK_MAX_DIFF_CHARS
    if len(diff) > MAX_CHARS:
        truncated_notice = (
            f"\n\n[DIFF TRUNCATED — exceeded {MAX_CHARS} characters. Some files may not be reviewed.]"
        )
        last_boundary = diff.rfind("\n\n--- a/", 0, MAX_CHARS - len(truncated_notice))
        cut_mid_file = not last_boundary > MAX_CHARS * 0.5
        if cut_mid_file:
            kept = diff[: MAX_CHARS - len(truncated_notice)]
        else:
            kept = diff[:last_boundary]
        diff = kept + truncated_notice

        # Name the casualties. Reporting only a character count is what once let a review
        # lose thirteen files silently: the run was green, and nothing said which ones
        # never reached the model.
        seen = re.findall(r"^\+\+\+ b/(.+)$", kept, re.MULTILINE)
        # Only the boundary branch ends on a file boundary. The fallback cuts mid-body, so the last
        # header in `kept` belongs to a file the model saw only part of — counting it as reviewed
        # would be the same silent-signal bug this change exists to remove.
        partial = seen[-1] if (cut_mid_file and seen) else None
        reviewed = set(seen) - ({partial} if partial else set())
        dropped = [f.filename for f in files if not is_generated_path(f.filename) and f.patch and f.filename not in reviewed]
        print(f"WARNING: Diff truncated ({len(diff)} chars). {len(dropped)} file(s) NOT fully reviewed:")
        for name in dropped:
            print(f"  - {name}{' (PARTIAL — cut mid-file)' if name == partial else ''}")

    if excluded:
        reclaimed = sum(size for _, _, size in excluded)
        print(f"Excluded {len(excluded)} generated file(s) from review, reclaiming {reclaimed} chars of budget:")
        for name, status, size in sorted(excluded, key=lambda item: -item[2]):
            print(f"  - {status} {name} ({size} chars)")
        # Announce them in the diff too, names and status only. Withholding the *fact* of the change
        # is a different thing from withholding its body: a reviewer that cannot see a deletion
        # happened will report the deletion as missing — a false finding this filter would cause.
        listed = "\n".join(f"  {status} {name}" for name, status, _ in sorted(excluded))
        diff += (
            f"\n\n[{len(excluded)} generated file(s) changed and were EXCLUDED from this diff. "
            "Listed so their presence is visible; their contents are machine-generated and are not "
            f"to be reviewed or commented on:\n{listed}\n]"
        )

    return Diff(text=diff, dropped=dropped, unreadable=False)


def call_deepseek(client, system_prompt, user_prompt, *, json_mode):
    """Call DeepSeek and return the raw response text.

    `json_mode` is the caller's decision and deliberately has no default. It used to be
    this helper's, hardcoded on, and that is the whole of legacy PR 57: the API refuses
    `response_format={"type": "json_object"}` unless the prompt contains the word "json" in
    some form. Mode A's system prompt satisfies that by construction ("Always respond with a
    JSON object"); Mode B has no such sentence, so a reply to a review comment succeeded or
    failed on whether the word happened to appear in the diff, in the thread, or in the
    developer's own sentence.

    Both outcomes shipped, and the successes are the reason it took so long to see. A refusal
    is a 400 and exit 1 on a `pull_request_review_comment` run, which attaches no check to the
    head commit — the pull request stays green and the developer simply never gets an answer
    (legacy PR 54 at 81e874f). An acceptance wraps the prose in an object, and the object is what
    gets posted: both bot replies on legacy PR 34 are still in their threads as literal
    `{"response": "…"}` and `{"body": "…"}`. That was read at the time as a formatting quirk.
    It is this same bug, succeeding instead of failing.

    Mode A's JSON contract is real and unchanged — it parses what comes back. Mode B is
    answering a human and never wanted a wrapper, so it asks for none. A third caller has to
    say which of the two it is; there is no default left to inherit by accident.
    """
    print(f"→ Calling DeepSeek ({DEEPSEEK_MODEL}) …{'' if json_mode else ' (prose)'}")
    # Omitted entirely rather than passed as None: the SDK serialises an explicit None into
    # the request body, which is not the same as never asking for a response format.
    json_contract = {"response_format": {"type": "json_object"}} if json_mode else {}
    try:
        resp = client.chat.completions.create(
            model=DEEPSEEK_MODEL,
            messages=[
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt},
            ],
            # Legacy PR 80 exhausted the old 65,536-token ceiling entirely in
            # reasoning_content while reviewing 50,963 diff characters. The
            # workflow owns this explicit ceiling; startup validation keeps it
            # within V4's documented provider maximum.
            max_tokens=DEEPSEEK_MAX_OUTPUT_TOKENS,
            # Where the caller's contract IS structured data, JSON mode makes it explicit
            # instead of relying on prompt wording alone. Where it is prose, this is absent —
            # see the docstring, and legacy PR 57.
            **json_contract,
            # Thinking on, effort tuned via reasoning_effort — both per the DeepSeek
            # V4 API docs; unknown extra fields are ignored by the endpoint, so this
            # stays compatible if a model variant lacks one of them.
            extra_body={
                "thinking": {"type": "enabled"},
                "reasoning_effort": DEEPSEEK_REASONING_EFFORT,
            },
        )
    except AuthenticationError as exc:
        hint = (
            "\n\nHINT: The DEEPSEEK_API_KEY was rejected by the DeepSeek API (401). "
            "Verify that:\n"
            "  1. DEEPSEEK_API_KEY is set in repo secrets (Settings → Secrets and variables → Actions)\n"
            "  2. The key is valid — create one at https://platform.deepseek.com/api_keys\n"
            "  3. Current base_url is https://api.deepseek.com"
        )
        print(f"ERROR: DeepSeek API authentication failed: {_sanitize_error(str(exc))}{hint}")
        sys.exit(1)
    except APITimeoutError:
        attempts = DEEPSEEK_MAX_RETRIES + 1
        print(
            f"ERROR: DeepSeek API call timed out after {DEEPSEEK_REQUEST_TIMEOUT_SECONDS:.0f}s "
            f"x {attempts} attempt(s).\n\n"
            "HINT: Generation time scales with the size of the diff, and this review had a large "
            "one (the byte count is printed as 'Diff: N chars' earlier in this log). Options:\n"
            "  1. Re-dispatch: bash scripts/gh-automation.sh pr-deepseek-force-review <pr>\n"
            "  2. Raise DEEPSEEK_REQUEST_TIMEOUT_SECONDS in deepseek-pr-review.yml — but the "
            "budget is that value times (DEEPSEEK_MAX_RETRIES + 1), and the product must stay "
            "below the job's timeout-minutes. Otherwise the job cap kills the step first and the "
            "run is reported as `cancelled` with no diagnostic at all.\n"
            "  3. Split the PR. A diff this size is also hard for a human to review in one pass."
        )
        sys.exit(1)
    except APIStatusError as exc:
        err = _sanitize_error(str(exc))
        hint = ""
        if exc.status_code == 402:
            hint = (
                "\n\nHINT: The DeepSeek account has insufficient balance (402). "
                "Top up at https://platform.deepseek.com/top_up and re-run the workflow."
            )
        elif exc.status_code == 429:
            hint = (
                "\n\nHINT: The DeepSeek API rate limit was hit (429). "
                "Re-run the workflow after a short wait."
            )
        print(f"ERROR: DeepSeek API call failed ({exc.status_code}): {err}{hint}")
        sys.exit(1)
    except Exception as exc:
        err = _sanitize_error(str(exc))
        hint = ""
        if "401" in err or "invalid" in err.lower() and "key" in err.lower():
            hint = (
                "\n\nHINT: The DEEPSEEK_API_KEY was rejected (401). "
                "Verify the key at https://platform.deepseek.com/api_keys"
            )
        elif "402" in err or "insufficient balance" in err.lower():
            hint = (
                "\n\nHINT: The DeepSeek account has insufficient balance (402). "
                "Top up at https://platform.deepseek.com/top_up"
            )
        print(f"ERROR: DeepSeek API call failed: {err}{hint}")
        sys.exit(1)

    choice = resp.choices[0]
    content = choice.message.content or ""

    # reasoning_content is the chain of thought, not the final answer. Legacy PR 14 hit
    # the token ceiling after 125K characters of reasoning and returned no content;
    # treating that as an empty, successful review made the workflow green without
    # publishing either an APPROVE or findings. Fail the run instead so the frozen
    # rule cannot mistake an absent verdict for a completed review.
    if not content:
        reasoning = getattr(choice.message, "reasoning_content", None) or ""
        finish_reason = getattr(choice, "finish_reason", None) or "unknown"
        raise RuntimeError(
            "DeepSeek returned no final review content "
            f"(model={DEEPSEEK_MODEL}, output_ceiling_tokens={DEEPSEEK_MAX_OUTPUT_TOKENS}, "
            f"finish_reason={finish_reason}, reasoning_chars={len(reasoning)}). "
            "No review verdict was published. " + _no_verdict_remedy(finish_reason)
        )

    usage = getattr(resp, "usage", None)
    completion_tokens = getattr(usage, "completion_tokens", None)
    usage_note = (
        f", completion_tokens={completion_tokens}"
        if isinstance(completion_tokens, int)
        else ""
    )
    print(f"← DeepSeek responded with {len(content)} chars{usage_note}")
    return content


def _extract_json(text):
    """Extract the outermost JSON object/array from text, handling markdown fences, preamble, and trailing text."""
    if not text:
        return ""
    # Find first { or [
    start = -1
    open_char = None
    close_char = None
    for i, ch in enumerate(text):
        if ch == "{":
            start = i
            open_char = "{"
            close_char = "}"
            break
        elif ch == "[":
            start = i
            open_char = "["
            close_char = "]"
            break
    if start == -1:
        return ""

    # Find matching closing bracket, tracking string escapes and bracket type
    depth = 0
    in_string = False
    escape = False
    for i in range(start, len(text)):
        ch = text[i]
        if escape:
            escape = False
            continue
        if ch == "\\":
            escape = True
            continue
        if ch == '"' and not in_string:
            in_string = True
            continue
        if ch == '"' and in_string:
            in_string = False
            continue
        if in_string:
            continue
        if ch == open_char:
            depth += 1
        elif ch == close_char:
            depth -= 1
            if depth == 0:
                return text[start : i + 1]
    return ""


def _strip_fences_fallback(text):
    """Simple regex-based fence stripping as fallback when bracket extraction fails."""
    t = text.strip()
    if t.startswith("```"):
        t = re.sub(r"^```[^\n]*\n?", "", t)
        t = re.sub(r"```\s*$", "", t)
    return t.strip()


# ─────────────────── Mode A: full review ───────────────────


def mode_full_review(client, repo, pr, bot_username):
    """PR opened or new commit pushed — run a complete diff review."""
    print("=== Mode A — Full Review ===")

    max_rounds = int(os.environ.get("MAX_ROUNDS", "1"))
    measure_only = os.environ.get("MEASURE_ONLY", "").strip().lower() in (
        "1",
        "true",
        "yes",
    )
    # Set by the workflow for manual dispatch: an explicitly requested review is
    # never what the cap is protecting against.
    forced = measure_only or os.environ.get("FORCE_REVIEW", "").strip().lower() in (
        "1",
        "true",
        "yes",
    )
    existing = _count_bot_reviews(pr, bot_username)
    print(f"Bot full reviews: {existing}/{max_rounds}{' (forced — cap bypassed)' if forced else ''}")
    if existing >= max_rounds and not forced:
        print("Review round limit reached — skipping full review.")
        _post_paused_notice(pr, existing, max_rounds, bot_username)
        return

    diff = get_diff(pr)
    if diff.unreadable:
        # Legacy PR 31: this used to fall into the branch below and report "nothing to review"
        # with a green check. A read failure is not an empty diff, and the review that
        # never happened must be visible as a failure rather than discovered by
        # unzipping the run's logs.
        raise RuntimeError(
            "The pull request diff could not be read, so no review was performed. "
            "The GitHub error is logged above."
        )
    if not diff.text:
        print("Empty diff — nothing to review.")
        return

    # len(diff) would be 3 — the field count of the Diff tuple, not the size of anything.
    # It printed exactly that for every pull request between legacy PRs 34 and 46, and reading it as
    # a three-character diff is what produced legacy PR 43.
    print(f"Diff: {len(diff.text)} chars across {pr.changed_files} files")

    sys_prompt = (
        "You are a senior code reviewer. You provide thorough, constructive feedback. "
        "You NEVER suggest creating commits, pushing code, or modifying files — you only review. "
        "You NEVER claim to have performed an action either — you cannot resolve a review thread, "
        "apply a label, merge, or change a file, so writing that you have is false. Saying what you "
        "examined is fine (\"I checked the callers\"); asserting a change to the pull request or to "
        "the code is not — describe what should change, never what you changed. "
        "Focus on SUBSTANTIVE issues only: bugs, security vulnerabilities, performance problems, "
        "architectural concerns, type safety gaps. SKIP minor style preferences, cosmetic formatting, "
        "and variable naming nitpicks unless they cause confusion. "
        "If the PR looks good and has no substantive issues, be willing to say so. "
        "Always respond with a JSON object: "
        '{"review_complete": <bool>, "comments": [{ "path": ..., "line": ..., "body": ... }, ...]}. '
        "Set review_complete=true ONLY when you believe the PR needs NO further review (no bugs, "
        "no security issues, no performance concerns). "
        "Set review_complete=false when there are substantive issues to address."
    )

    user_prompt = f"""Review the following pull-request diff carefully.

PR title: {pr.title}
PR description:
{pr.body or "(none)"}

Diffs:
{diff.text}

Return a JSON object with two fields:
  "review_complete": true if the PR needs NO further review (no substantive issues remain), false otherwise
  "comments": an array of review comments, each with:
    {{ "path": "<file path or null>", "line": <number or null>, "body": "<review comment>" }}

Rules:
- ANCHOR EVERY FINDING YOU CAN. If a finding names a file, a function, a type or a
  line — anything a reader would have to open a file to check — it MUST carry that
  file's path and the line number on the new-file side. Findings delivered as
  general comments create no review thread on GitHub, so nobody can resolve them
  and the pull request stalls waiting for a human to acknowledge prose.
- path=null + line=null ONLY for an observation that genuinely belongs to no single
  file: a repository-wide concern, or a remark about the change as a whole. If you
  can name the file it is about, it is not one of these.
- For line-specific comments, include the correct file path and line number (new-file side)
- Be constructive and specific. Include corrected code snippets in markdown code blocks.
- Only flag SUBSTANTIVE issues (bugs, security, performance, architectural, type-safety)
- Do NOT nitpick minor formatting, style, or naming preferences
- If there are genuinely no substantive issues, set review_complete=true and return an empty comments array
- Output ONLY the JSON object — no preamble, no markdown around it."""

    # JSON mode: the prompts above ask for an object and _parse_review_response expects one.
    # This is the mode whose prompt carries the word the API requires; keep it that way.
    raw = call_deepseek(client, sys_prompt, user_prompt, json_mode=True)
    review_complete, comments = _parse_review_response(raw)

    # A partial read cannot produce a clean verdict, and it cannot be allowed to look
    # like one (legacy PR 32). On legacy PR 30 the budget ran out after the client files, so the entire
    # server half — collision, intent intake, the speed clamp, the snapshot broadcast —
    # was never seen, and the review said "no substantive issues found". `pr-status` then
    # passed the frozen rule.
    #
    # Injecting the skipped files as a finding rather than adding a second posting path
    # is deliberate: it reuses the machinery that already stamps the round marker and
    # already makes prose in the body count as unread findings, so the pull request
    # blocks until a human acknowledges what was not reviewed. Which files get dropped is
    # not random either — the budget fills in file order, so the same directories lose
    # every time, and saying which ones is the whole point.
    if diff.dropped:
        # Every file, not a sample. A cap of twenty would have hidden exactly what this
        # notice exists to expose: the budget fills in file order, so a systematically
        # skipped directory tends to sit *after* the first twenty names, which is the case
        # a truncated list quietly drops.
        #
        # The one bound left is GitHub's, not a preference: a review body is limited, and a
        # pull request may carry up to 3000 files. When the list cannot fit, the number of
        # unnamed files and the reason are stated, so the limit is visible rather than
        # silently applied.
        NOTICE_BUDGET = 40_000
        listed, omitted = [], 0
        used = 0
        for name in diff.dropped:
            entry = f"- `{name}`"
            if used + len(entry) + 1 > NOTICE_BUDGET:
                omitted = len(diff.dropped) - len(listed)
                break
            listed.append(entry)
            used += len(entry) + 1
        listed = "\n".join(listed)
        if omitted:
            listed += (
                f"\n- …and {omitted} more, unnamed only because the list would exceed the "
                "review body's size limit"
            )
        comments.insert(0, {
            "path": None,
            "line": None,
            "body": (
                f"**This review is incomplete.** The diff exceeded the review budget, so "
                f"{len(diff.dropped)} of the changed files were not fully read and nothing below "
                f"says anything about them:\n\n{listed}\n\n"
                "Whatever the rest of this review does or does not report, it is not a verdict on "
                "those files. Splitting the pull request is the reliable way to get them reviewed."
            ),
        })
        print(f"Injected a truncation notice naming {len(diff.dropped)} unreviewed file(s)")

    # `review_complete=false` means there are substantive issues to report. An
    # empty list cannot satisfy that contract and must not be allowed to end the
    # workflow successfully without creating a review, as happened on legacy PR 14.
    if not review_complete and not comments:
        raise RuntimeError(
            "DeepSeek returned no actionable review verdict: review_complete is false "
            "but comments is empty."
        )

    # A manual workflow dispatch can exercise the real model and parser against a
    # production-sized PR without publishing feedback to that PR. This is a
    # measurement path, not a review round: it intentionally creates no GitHub
    # review, marker, thread, label or paused notice.
    if measure_only:
        print(
            "MEASURE ONLY — parsed final review content successfully "
            f"(review_complete={review_complete}, comments={len(comments)}); "
            "nothing was posted to GitHub."
        )
        return

    if not comments and review_complete:
        # Posted as a COMMENT, not an APPROVE, and not because a comment is tidier:
        # GitHub forbids Actions from approving pull requests, and the PAT is no way
        # out either because nobody may approve their own PR. On a repository with one
        # human author the APPROVE path is unreachable, so attempting it turned every
        # flawless PR into a failed job (legacy PR 22).
        #
        # The marker is what carries the meaning instead of the review state. It must
        # be the very first thing in the body: gh-automation.sh exempts this review
        # from the unread-findings count only when the body *begins* with the marker
        # and does not carry the full-review marker, so a review with findings can
        # never impersonate a clean one.
        #
        # Deliberately unstamped with FULL_REVIEW_MARKER: that marker is what
        # bot_review_count counts, and a clean pass must not spend the one-round
        # budget — a later push has to remain reviewable.
        print("DeepSeek signals review complete with no comments — posting the clean verdict.")
        try:
            pr.create_review(
                body=(
                    f"{NO_FINDINGS_MARKER}\n\n"
                    "DeepSeek review complete: no substantive issues found."
                ),
                event="COMMENT",
            )
            print("✓ Clean verdict posted as a COMMENT review")
        except GithubException as exc:
            print(f"ERROR posting the clean verdict: {exc}")
            raise RuntimeError(
                "DeepSeek produced a clean verdict, but GitHub rejected the review that records it."
            ) from exc
        return

    inline = [c for c in comments if c.get("path") and isinstance(c.get("line"), (int, float))]
    general = [c for c in comments if not c.get("path") or not isinstance(c.get("line"), (int, float))]

    print(f"Parsed {len(inline)} inline + {len(general)} general comments (review_complete={review_complete})")

    pr_file_paths = set()
    try:
        pr_file_paths = {f.filename for f in pr.get_files()}
    except Exception as e:
        print(f"WARNING: Could not fetch PR files for path validation: {e}")
        print("Skipping path validation — relying on GitHub API to reject invalid paths")

    review_comments = []
    for c in inline:
        path = c.get("path", "")
        if pr_file_paths and path not in pr_file_paths:
            print(f"WARNING: File '{path}' not in PR changes — treating as general comment")
            general.append({"path": path, "line": c.get("line"), "body": c["body"]})
            continue
        line = _safe_int(c.get("line"))
        if line is None:
            print(f"WARNING: Invalid line number {c.get('line')!r} for {c.get('path')}, treating as general")
            general.append({"path": c.get("path", ""), "line": c.get("line"), "body": c["body"]})
            continue
        review_comments.append({
            "path": c["path"],
            "line": line,
            "side": "RIGHT",
            "body": c["body"],
        })

    body = ""
    if general:
        body = "## General Comments\n\n" + "\n\n---\n\n".join(
            f"*{i + 1}.* {g['body']}" for i, g in enumerate(general)
        )

    if not review_comments and not body:
        raise RuntimeError(
            "DeepSeek comments could not be converted into a GitHub review. "
            "No review verdict was published."
        )

    # Never APPROVE, whatever the verdict. GitHub forbids Actions from approving pull
    # requests, and this branch runs when the model reported comments — so an APPROVE
    # here failed the whole run for the "complete, with observations" case exactly as it
    # did for the clean one (legacy PR 22).
    #
    # A verdict with observations therefore lands as a stamped COMMENT, which does
    # consume the round. That is the honest accounting: a full review happened, and the
    # observations are content a human must still read — the frozen rule counts them as
    # unread findings until the DEEPSEEK_REVIEW_READ label says otherwise.
    event_type = "COMMENT"

    try:
        # PyGithub's create_review asserts comments is a list — pass [] instead of None
        posted_body = _stamp(body)
        pr.create_review(body=posted_body, event=event_type, comments=review_comments or [])
        print(f"✓ Review posted as {event_type} — {len(review_comments)} inline comments")
    except GithubException as exc:
        print(f"ERROR posting review: {exc}")
        # If line-based posting fails, convert inline to general and post as body-only
        if "line" in str(exc).lower() or "position" in str(exc).lower():
            print("Converting inline comments to general due to line/position error")
            for c in review_comments:
                general.append({
                    "path": c["path"],
                    "line": str(c["line"]),
                    "body": c["body"],
                })
            body = "## General Comments\n\n" + "\n\n---\n\n".join(
                f"*{i + 1}.* In `{g['path']}:{g.get('line', '?')}`:\n\n{g['body']}"
                for i, g in enumerate(general)
            )
            try:
                pr.create_review(body=_stamp(body), event=event_type)
                print(f"✓ Review posted as general-only {event_type} — {len(general)} items")
            except GithubException as exc2:
                print(f"ERROR on fallback post: {exc2}")
        else:
            raise


# ──────────────── Mode B: reply to review comment ────────────────


def mode_reply(client, repo, pr, comment_body, comment_id, comment_author, bot_username):
    """Someone commented on a PR review — reply if the parent was authored by the bot."""

    # Anti-loop: never respond to our own comments
    if comment_author == bot_username:
        print("Skipping — comment author is the bot (anti-loop).")
        return

    try:
        triggering_comment = pr.get_review_comment(comment_id)
    except GithubException as exc:
        print(f"ERROR fetching triggering comment #{comment_id}: {exc}")
        return

    parent_id = getattr(triggering_comment, "in_reply_to_id", None)
    if not parent_id:
        print("Comment is not a reply — skipping.")
        return

    try:
        parent_comment = pr.get_review_comment(parent_id)
    except GithubException as exc:
        print(f"ERROR fetching parent comment #{parent_id}: {exc}")
        return

    # Walk reply chain to find if any ancestor is from the bot
    ancestor = _find_bot_ancestor(pr, parent_comment, bot_username)
    if ancestor is None:
        print(f"Parent comment author is '{parent_comment.user.login}', not bot — skipping.")
        return

    print(f"=== Mode B — Reply to bot comment #{ancestor.id} ===")

    # Gather full context (smaller diff for reply mode to manage token usage)
    fetched = get_diff(pr)
    if fetched.unreadable:
        # A reply written without the diff would be a guess presented as an answer.
        raise RuntimeError(
            "The pull request diff could not be read, so no reply was written. "
            "The GitHub error is logged above."
        )
    diff = fetched.text
    REPLY_DIFF_MAX = 60_000
    if len(diff) > REPLY_DIFF_MAX:
        diff = diff[:REPLY_DIFF_MAX] + "\n\n[DIFF TRUNCATED for reply context]"

    # A reply is worth writing from a partial diff — the developer asked about one thread,
    # not about the whole change — but it must not be written in ignorance of what is
    # missing. Refusing outright would break the conversation feature to prevent an
    # answer that is usually fine; telling the model what it cannot see costs a paragraph
    # and lets it say "I cannot see that file" instead of guessing (legacy PR 32's failure mode,
    # arriving through the other door).
    if fetched.dropped:
        diff += (
            f"\n\n[{len(fetched.dropped)} changed file(s) are NOT in this diff at all, because it "
            "exceeded the review budget. Do not draw conclusions about them, and say so plainly if "
            "the question depends on one:\n"
            + "\n".join(f"  {name}" for name in fetched.dropped)
            + "\n]"
        )
        print(f"Reply context is missing {len(fetched.dropped)} file(s); told the model which")

    threads_context = ""
    try:
        # Fetch only recent comments to bound context
        all_review_comments = list(pr.get_review_comments()[:50])
        threads_context = "Recent review comment threads (last 50):\n"
        for c in all_review_comments:
            author = c.user.login
            in_reply = getattr(c, "in_reply_to_id", None)
            threads_context += (
                f"  [{author}] on {c.path}:{c.line} "
                f"(id={c.id}, in_reply_to={in_reply}): {c.body[:400]}\n"
            )
    except GithubException:
        threads_context = "(could not fetch review comments)"

    # The three writes this script can make are `create_issue_comment`, `create_review` and
    # the `create_review_comment_reply` below. Resolving a thread is not among them, and on
    # PR #209 a reply announced one anyway — 72 seconds after a human had already resolved it.
    # The two clauses that existed forbade *suggesting* an action; a false claim is not a
    # suggestion, and it is worse, because a suggestion invites a decision and a claim
    # forecloses one. The clause below closes that gap without muzzling a reviewer who says
    # what it looked at, which is a legitimate and useful thing for a reply to say (#212).
    sys_prompt = (
        "You are a senior code reviewer responding to a developer's reply. "
        "Be helpful, constructive, and concise. "
        "If you suggest a code fix, ALWAYS include the corrected code snippet in a markdown code block — "
        "NEVER suggest making a commit, pushing, or modifying files. You are a reviewer only. "
        "NEVER claim to have performed an action either — you cannot resolve or close this thread, "
        "apply a label, merge, or change a file, so writing that you have is false. Saying what you "
        "examined is fine (\"I checked the callers\"); asserting a change to the thread or to the code "
        "is not. Recommend, never announce: \"I agree, this thread can be closed\" is correct, "
        "\"I'm closing this thread as resolved\" is false."
    )

    user_prompt = f"""A developer replied to your review comment. Respond thoughtfully.

PR title: {pr.title}
PR description:
{pr.body or "(none)"}

Your original comment (by {bot_username} on {parent_comment.path}:{parent_comment.line}):
{parent_comment.body}

Developer's reply:
{comment_body}

{threads_context}

Full diff for reference:
{diff}

Respond directly to the developer. If you agree, say so. If you disagree, explain why politely.
When suggesting a code fix, include a markdown code block with the corrected snippet.
Do NOT suggest git commands, commits, or file modifications — only provide the corrected code.
Do NOT claim to have performed an action — you cannot resolve this thread, label it, merge, or
change a file. Recommend, never announce."""

    # No JSON mode: this answer goes to a human, verbatim, in the review thread. Asking for
    # an object here is what put `{"response": "…"}` in legacy PR 34's threads and what 400s
    # whenever the word "json" is absent from everything above (legacy PR 57).
    reply_text = call_deepseek(client, sys_prompt, user_prompt, json_mode=False)

    try:
        pr.create_review_comment_reply(comment_id, reply_text)
        print("✓ Reply posted")
    except GithubException as exc:
        print(f"ERROR posting reply: {exc}")


def _find_bot_ancestor(pr, comment, bot_username, depth=0):
    """Walk up reply chain to find if any ancestor comment is from the bot."""
    if depth > 10:
        return None
    if comment.user.login == bot_username:
        return comment
    parent_id = getattr(comment, "in_reply_to_id", None)
    if not parent_id:
        return None
    try:
        parent = pr.get_review_comment(parent_id)
        return _find_bot_ancestor(pr, parent, bot_username, depth + 1)
    except GithubException:
        return None


# ──────────────────── JSON parsing ────────────────────


def _repair_json(text):
    """Repair common LLM JSON issues: unbalanced brackets, trailing commas, unescaped newlines.

    Returns the parsed result on success or None if unrepairable.
    """
    if not text:
        return None

    attempts = []
    original = text

    # Stage 1: escape literal control characters inside JSON string values
    control_fixed = _escape_control_chars_in_json_strings(original)
    if control_fixed != original:
        attempts.append(control_fixed)

    # Stage 2: remove trailing commas before } or ]
    comma_fixed = re.sub(r",\s*([}\]])", r"\1", control_fixed)
    if comma_fixed != control_fixed:
        attempts.append(comma_fixed)

    # Stage 3: balance unclosed brackets using a stack-aware suffix
    suffix = _closing_suffix_for_json(comma_fixed)
    if suffix:
        # Close any unclosed string first, then append brackets
        repair_a = comma_fixed + '"' + suffix
        repair_b = comma_fixed + suffix
        attempts.append(repair_a)
        attempts.append(repair_b)

    # Always try the most repaired version and the original
    attempts.append(comma_fixed)
    attempts.append(original)

    seen = set()
    for attempt in attempts:
        if not attempt or attempt in seen:
            continue
        seen.add(attempt)
        try:
            return json.loads(attempt)
        except json.JSONDecodeError:
            continue

    return None


def _escape_control_chars_in_json_strings(text):
    """Escape literal newlines/tabs inside JSON string values without touching already-escaped ones."""
    result = []
    in_string = False
    escape = False
    i = 0
    while i < len(text):
        ch = text[i]
        if escape:
            result.append(ch)
            escape = False
            i += 1
            continue
        if ch == "\\":
            result.append(ch)
            escape = True
            i += 1
            continue
        if ch == '"' and not in_string:
            in_string = True
            result.append(ch)
            i += 1
            continue
        if ch == '"' and in_string:
            in_string = False
            result.append(ch)
            i += 1
            continue
        if in_string and ch in "\n\r\t":
            if ch == "\n":
                result.append("\\n")
            elif ch == "\r":
                result.append("\\r")
            elif ch == "\t":
                result.append("\\t")
            i += 1
            continue
        result.append(ch)
        i += 1
    return "".join(result)


def _closing_suffix_for_json(text):
    """Return the smallest suffix that balances JSON brackets, tracking strings."""
    stack = []
    in_string = False
    escape = False
    for ch in text:
        if escape:
            escape = False
            continue
        if ch == "\\":
            escape = True
            continue
        if ch == '"':
            in_string = not in_string
            continue
        if in_string:
            continue
        if ch == "{":
            stack.append("}")
        elif ch == "[":
            stack.append("]")
        elif ch in "}]":
            if stack and stack[-1] == ch:
                stack.pop()
    return "".join(reversed(stack))


def _fallback_parse_review_response(raw):
    """When strict JSON parsing fails, try to recover individual comment objects loosely."""
    text = _extract_json(raw) or _strip_fences_fallback(raw)
    if not text:
        return False, []

    review_complete = bool(re.search(r'"review_complete"\s*:\s*(true|True|TRUE)', text))

    comments = []
    for chunk in _loose_comment_chunks(text):
        path = _extract_json_string_field(chunk, "path") or None
        line = _extract_json_number_field(chunk, "line")
        body = _extract_body_field(chunk)
        if body is not None:
            comments.append({"path": path, "line": line, "body": body})

    return review_complete, comments


def _loose_comment_chunks(text):
    """Split text into probable comment object chunks by looking for path fields."""
    path_positions = [m.start() for m in re.finditer(r'"path"', text)]
    starts = []
    for pos in path_positions:
        start = text.rfind("{", 0, pos)
        starts.append(start if start >= 0 else pos)
    ends = starts[1:] + [len(text)]
    chunks = []
    for s, e in zip(starts, ends):
        chunk = text[s:e].rstrip(", \n\r")
        if not chunk.endswith("}"):
            chunk += "}"
        chunks.append(chunk)
    return chunks


def _extract_json_string_field(chunk, key):
    m = re.search(rf'"{re.escape(key)}"\s*:\s*"((?:[^"\\]|\\.)*)"', chunk)
    if m:
        return _decode_jsonish_string(m.group(1))
    if re.search(rf'"{re.escape(key)}"\s*:\s*null', chunk):
        return None
    return None


def _extract_json_number_field(chunk, key):
    m = re.search(rf'"{re.escape(key)}"\s*:\s*(\d+)', chunk)
    if m:
        return int(m.group(1))
    if re.search(rf'"{re.escape(key)}"\s*:\s*null', chunk):
        return None
    return None


def _extract_body_field(chunk):
    m = re.search(r'"body"\s*:\s*"', chunk)
    if not m:
        return None
    start = m.end()

    # Collect all unescaped quote positions as candidate closing quotes
    candidates = []
    i = start
    while i < len(chunk):
        if chunk[i] == "\\":
            i += 2
            continue
        if chunk[i] == '"':
            candidates.append(i)
        i += 1

    # Prefer the last candidate whose remainder looks like the end of an object
    for idx in reversed(candidates):
        remainder = chunk[idx + 1 :].strip()
        if remainder.startswith(("}", "},")):
            return _decode_jsonish_string(chunk[start:idx])

    # Fallback to the last quote if none match the expected pattern
    if candidates:
        return _decode_jsonish_string(chunk[start : candidates[-1]])

    return _decode_jsonish_string(chunk[start:])


def _decode_jsonish_string(value):
    """Decode common JSON escapes in a raw string segment."""
    value = value.replace('\\"', '"')
    value = value.replace("\\\\", "\\")
    value = value.replace("\\n", "\n")
    value = value.replace("\\r", "\r")
    value = value.replace("\\t", "\t")
    return value


def _parse_review_response(raw):
    """Parse DeepSeek response: {'review_complete': bool, 'comments': [...]} or bare array (backward compat)."""
    # Try bracket-based extraction first (handles fences, preamble, trailing text)
    text = _extract_json(raw)

    # Fallback: simple regex fence stripping (handles truncated JSON where
    # brackets never close, or edge cases _extract_json misses)
    if not text:
        brace_count = raw.count("{") + raw.count("[")
        close_count = raw.count("}") + raw.count("]")
        if brace_count > 0:
            print(f"Bracket extraction failed: found {brace_count} openers but {close_count} closers — "
                  f"possible unclosed brackets or mismatched types")
        text = _strip_fences_fallback(raw)

    if not text:
        print(f"Could not extract JSON from response (first 300 / last 200 chars):")
        print(f"  START: {raw[:300]}")
        print(f"  END:   {raw[-200:]}")
        return False, []

    try:
        result = json.loads(text)
        if isinstance(result, dict) and "comments" in result:
            return result.get("review_complete", False), result["comments"]
        elif isinstance(result, list):
            return False, result
        else:
            print(f"Unexpected response format: {type(result).__name__}")
            return False, []
    except json.JSONDecodeError as exc:
        print(f"JSON parse failed: {exc}")
        # Last resort: attempt automatic repair of common LLM JSON issues
        repaired = _repair_json(text)
        if repaired is not None:
            print("JSON repaired successfully after automatic fix")
            if isinstance(repaired, dict) and "comments" in repaired:
                return repaired.get("review_complete", False), repaired["comments"]
            elif isinstance(repaired, list):
                return False, repaired
            else:
                print(f"Repaired result is unexpected type: {type(repaired).__name__}")
                return False, []
        print("JSON repair failed — attempting loose fallback recovery")
        return _fallback_parse_review_response(raw)


if __name__ == "__main__":
    main()
