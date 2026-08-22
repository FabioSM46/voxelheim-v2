#!/usr/bin/env python3
"""Regression tests for DeepSeek review response parsing and diff assembly."""

import importlib.util
import sys
import types
import contextlib
import inspect
import io
import json
import re
import unittest
from unittest import mock
from pathlib import Path


def _install_dependency_stubs():
    github_module = types.ModuleType("github")
    github_module.Auth = types.SimpleNamespace(Token=lambda token: token)
    github_module.Github = object
    github_module.GithubException = Exception
    sys.modules.setdefault("github", github_module)

    # Every name deepseek_review.py imports from `openai` must be stubbed here. Missing one is
    # not a soft failure — the import fails and the whole module refuses to load, taking every
    # test in this file with it (clinic-deck learned this when an added `APITimeoutError` import
    # silently killed its suite until CI started running it).
    openai_module = types.ModuleType("openai")
    openai_module.APIStatusError = Exception
    openai_module.APITimeoutError = Exception
    openai_module.AuthenticationError = Exception
    openai_module.OpenAI = object
    sys.modules.setdefault("openai", openai_module)


def _load_deepseek_review():
    _install_dependency_stubs()
    script_path = Path(__file__).with_name("deepseek_review.py")
    spec = importlib.util.spec_from_file_location("deepseek_review", script_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


deepseek_review = _load_deepseek_review()

# Fixture sizes are derived, never guessed: a diff cap that changes must not quietly
# stop these truncation tests from truncating anything. That is precisely what happened
# when the cap moved — seven tests kept passing the wrong thing until they were run.
_CAP = deepseek_review.DEEPSEEK_MAX_DIFF_CHARS

# The login the workflow runs as, and therefore the author of every comment the bot owns.
_BOT_USERNAME = "github-actions[bot]"


class DiffCapTests(unittest.TestCase):
    """The cap is a measured number, and these are the measurements.

    Two caps before this one described the model's context window, which is not what
    bounds a review — the chain of thought is emitted into the same output budget the
    verdict needs, so a diff can be far inside the context and still leave no room to
    answer. Both were defended by a claim about the world and neither was pinned to an
    observation, so both went on being wrong after the world changed (#167).
    """

    # Measured on deepseek-v4-flash at reasoning_effort=max. Characters of diff, and
    # whether a verdict came back.
    _OBSERVED_FAILURE = 124_711  # PR #164: 1,481,442 reasoning chars, finish_reason=length
    _OBSERVED_SUCCESS = 72_350  # PR #169: the largest diff a verdict has come back for

    def test_the_cap_is_below_the_diff_that_produced_no_verdict(self):
        self.assertLess(
            deepseek_review.DEEPSEEK_MAX_DIFF_CHARS,
            self._OBSERVED_FAILURE,
            "the cap must keep a diff smaller than one measured to exhaust the output "
            "budget; above it the guard cannot fire before the API call does",
        )

    def test_the_cap_is_not_below_a_diff_that_was_reviewed_whole(self):
        self.assertGreaterEqual(
            deepseek_review.DEEPSEEK_MAX_DIFF_CHARS,
            self._OBSERVED_SUCCESS,
            "a cap under a diff the model has actually reviewed would truncate work "
            "nothing measured says it cannot do",
        )

    def test_the_cap_leaves_the_reasoning_room_to_finish(self):
        """The arithmetic the constant's comment states, checked rather than asserted.

        1,481,442 characters emitted for 384,000 tokens is 3.86 characters per token, and
        the model reasons about 11.9 characters per character of diff.

        **What the headroom is for is the reasoning, not the verdict.** A verdict is small
        — #80 returned 1,060 final characters, about 275 tokens — and a margin sized on it
        would be no margin at all. The number that has to fit with room to spare is the
        reasoning itself, because the ratio above is an average over two observations and a
        tangled diff reasons harder than a straightforward one. A quarter of the ceiling
        left over is the margin this cap claims; anything less and the cap is a coin toss.
        """
        chars_per_token = 1_481_442 / 384_000
        reasoning_per_diff_char = 1_481_442 / self._OBSERVED_FAILURE
        reasoning_tokens = (
            deepseek_review.DEEPSEEK_MAX_DIFF_CHARS
            * reasoning_per_diff_char
            / chars_per_token
        )
        ceiling = deepseek_review.DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS
        self.assertLess(
            reasoning_tokens,
            0.75 * ceiling,
            f"a diff at the cap reasons for about {reasoning_tokens:,.0f} of {ceiling:,} "
            "tokens, which leaves too little for a diff that reasons harder than the "
            "average this ratio was measured on",
        )


class NoVerdictRemedyTests(unittest.TestCase):
    """What the run tells an operator when nothing came back.

    The advice this replaces named a lever that did not move: raise the output ceiling,
    when the ceiling was already the provider's maximum. A diagnostic somebody makes a
    decision from is an output, and outputs are pinned here.
    """

    def test_at_the_provider_ceiling_it_does_not_ask_for_more_budget(self):
        with mock.patch.object(
            deepseek_review,
            "DEEPSEEK_MAX_OUTPUT_TOKENS",
            deepseek_review.DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS,
        ):
            remedy = deepseek_review._no_verdict_remedy("length")
        self.assertIn("no budget to raise", remedy)
        self.assertNotIn("Raise DEEPSEEK_MAX_OUTPUT_TOKENS", remedy)
        self.assertIn("DEEPSEEK_MAX_DIFF_CHARS", remedy)

    def test_below_the_provider_ceiling_it_asks_for_more_budget(self):
        with mock.patch.object(deepseek_review, "DEEPSEEK_MAX_OUTPUT_TOKENS", 65_536):
            remedy = deepseek_review._no_verdict_remedy("length")
        self.assertIn("Raise DEEPSEEK_MAX_OUTPUT_TOKENS", remedy)

    def test_a_finish_that_is_not_length_is_not_a_budget_problem(self):
        remedy = deepseek_review._no_verdict_remedy("stop")
        self.assertIn("not an output-budget failure", remedy)
        self.assertNotIn("DEEPSEEK_MAX_OUTPUT_TOKENS", remedy)


class OutputBudgetConfigurationTests(unittest.TestCase):
    def test_missing_configuration_uses_the_documented_default(self):
        self.assertEqual(
            deepseek_review.DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS,
            deepseek_review._read_output_token_budget({}),
        )
        self.assertEqual(384_000, deepseek_review.DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS)

    def test_valid_configuration_is_read_as_an_integer(self):
        self.assertEqual(
            200_000,
            deepseek_review._read_output_token_budget(
                {"DEEPSEEK_MAX_OUTPUT_TOKENS": "200000"}
            ),
        )

    def test_invalid_configuration_fails_closed(self):
        invalid = {
            "zero": "0",
            "negative": "-1",
            "non-numeric": "many",
            "fractional": "131072.0",
            "provider-oversized": str(
                deepseek_review.DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS + 1
            ),
        }

        for label, value in invalid.items():
            with self.subTest(case=label):
                with self.assertRaisesRegex(ValueError, "DEEPSEEK_MAX_OUTPUT_TOKENS"):
                    deepseek_review._read_output_token_budget(
                        {"DEEPSEEK_MAX_OUTPUT_TOKENS": value}
                    )

    def test_provider_limit_itself_is_accepted(self):
        limit = deepseek_review.DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS

        self.assertEqual(
            limit,
            deepseek_review._read_output_token_budget(
                {"DEEPSEEK_MAX_OUTPUT_TOKENS": str(limit)}
            ),
        )


class ParseReviewResponseTests(unittest.TestCase):
    def test_parses_valid_review_json(self):
        raw = '''{
          "review_complete": false,
          "comments": [
            {
              "path": "server/internal/world/chunk.go",
              "line": 24,
              "body": "This chunk lookup is O(n) per voxel; use the index map."
            }
          ]
        }'''

        review_complete, comments = deepseek_review._parse_review_response(raw)

        self.assertFalse(review_complete)
        self.assertEqual(1, len(comments))
        self.assertEqual("server/internal/world/chunk.go", comments[0]["path"])
        self.assertEqual(24, comments[0]["line"])

    def test_repairs_response_missing_outer_closing_brackets(self):
        raw = '''{
          "review_complete": false,
          "comments": [
            {
              "path": "server/internal/world/chunk.go",
              "line": 24,
              "body": "Use the index map instead."
            }
        '''

        review_complete, comments = deepseek_review._parse_review_response(raw)

        self.assertFalse(review_complete)
        self.assertEqual(1, len(comments))
        self.assertEqual("Use the index map instead.", comments[0]["body"])

    def test_recovers_comments_from_json_with_unescaped_body_quotes(self):
        raw = '''{
          "review_complete": false,
          "comments": [
            {
              "path": "server/internal/world/chunk.go",
              "line": 24,
              "body": "**Performance issue.** The snippet `key := fmt.Sprintf("%d", x)` breaks strict JSON.

Use a struct key instead."
            }
          ]
        }'''

        review_complete, comments = deepseek_review._parse_review_response(raw)

        self.assertFalse(review_complete)
        self.assertEqual(1, len(comments))
        self.assertEqual("server/internal/world/chunk.go", comments[0]["path"])
        self.assertEqual(24, comments[0]["line"])
        self.assertIn('fmt.Sprintf("%d", x)', comments[0]["body"])
        self.assertIn("Use a struct key instead.", comments[0]["body"])


class CallDeepSeekContractTests(unittest.TestCase):
    class _Completions:
        def __init__(self, response):
            self.response = response
            self.kwargs = None

        def create(self, **kwargs):
            self.kwargs = kwargs
            return self.response

    @staticmethod
    def _response(content, reasoning_content="", finish_reason="stop"):
        message = types.SimpleNamespace(
            content=content,
            reasoning_content=reasoning_content,
        )
        choice = types.SimpleNamespace(message=message, finish_reason=finish_reason)
        return types.SimpleNamespace(choices=[choice])

    def test_requests_json_output_with_room_for_the_final_verdict(self):
        completions = self._Completions(
            self._response('{"review_complete": true, "comments": []}')
        )
        client = types.SimpleNamespace(
            chat=types.SimpleNamespace(completions=completions)
        )

        result = deepseek_review.call_deepseek(client, "system", "user", json_mode=True)

        self.assertIn('"review_complete": true', result)
        self.assertEqual("deepseek-v4-flash", completions.kwargs["model"])
        self.assertEqual({"type": "json_object"}, completions.kwargs["response_format"])
        self.assertEqual(
            deepseek_review.DEEPSEEK_MAX_OUTPUT_TOKENS,
            completions.kwargs["max_tokens"],
        )
        self.assertEqual(
            {
                "thinking": {"type": "enabled"},
                "reasoning_effort": "max",
            },
            completions.kwargs["extra_body"],
        )

    def test_empty_final_content_fails_even_when_reasoning_is_present(self):
        completions = self._Completions(
            self._response("", reasoning_content="x" * 125_162, finish_reason="length")
        )
        client = types.SimpleNamespace(
            chat=types.SimpleNamespace(completions=completions)
        )

        with self.assertRaises(RuntimeError) as raised:
            deepseek_review.call_deepseek(client, "system", "user", json_mode=True)

        diagnostic = str(raised.exception)
        self.assertIn("model=deepseek-v4-flash", diagnostic)
        self.assertIn(
            f"output_ceiling_tokens={deepseek_review.DEEPSEEK_MAX_OUTPUT_TOKENS}",
            diagnostic,
        )
        self.assertIn("finish_reason=length", diagnostic)
        self.assertIn("reasoning_chars=125162", diagnostic)
        # The remedy is the ceiling's, not a fixed sentence. At the provider maximum —
        # which is where this build runs — "raise the budget" names a lever that does not
        # move, and this assertion used to pin exactly that advice. See NoVerdictRemedyTests.
        self.assertIn("no budget to raise", diagnostic)
        self.assertNotIn("x" * 100, diagnostic, "reasoning content must never be logged")

    def test_success_log_records_final_content_and_completion_token_sizes(self):
        response = self._response('{"review_complete": true, "comments": []}')
        response.usage = types.SimpleNamespace(completion_tokens=73_421)
        completions = self._Completions(response)
        client = types.SimpleNamespace(
            chat=types.SimpleNamespace(completions=completions)
        )

        with contextlib.redirect_stdout(io.StringIO()) as output:
            deepseek_review.call_deepseek(client, "system", "user", json_mode=True)

        self.assertIn("41 chars", output.getvalue())
        self.assertIn("completion_tokens=73421", output.getvalue())


class _FakeFile:
    def __init__(self, filename, patch, status="modified", additions=None, deletions=0):
        self.filename = filename
        self.patch = patch
        self.status = status
        self.previous_filename = None
        # Defaults mirror GitHub: a file with a patch reports the lines it changed, and a
        # binary file reports none. Tests that care set them explicitly.
        self.additions = len(patch or "") if additions is None else additions
        self.deletions = deletions


class _FakePR:
    def __init__(self, files):
        self._files = files

    def get_files(self):
        return self._files


def _patch_of(chars, marker="x"):
    """A patch body of roughly `chars` characters, tagged so tests can identify it."""
    return f"@@ -1 +1 @@\n+{marker * max(chars, 1)}"


class GetDiffExcludesGeneratedArtifactsTests(unittest.TestCase):
    """
    Generated FlatBuffers bindings must never consume the review budget.

    The clinic-deck ancestor of this rule watched generated artifacts crowd thirteen
    real source files out of a review's 120,000-char budget — the run was green and
    nothing said which files never reached the model. Both sides of the contract
    vendor flatc output under a `gen/` path segment (Rust additionally carries the
    `_generated.` infix), and those diffs are excluded by name while their presence
    is still announced.
    """

    def test_generated_bindings_never_reach_the_reviewer(self):
        pr = _FakePR(
            [
                _FakeFile("server/internal/gen/fbs/Chunk.go", _patch_of(500, "m")),
                _FakeFile("client/src/net/gen/chunk_generated.rs", _patch_of(500, "r")),
                _FakeFile("server/internal/world/chunk.go", _patch_of(50, "s")),
            ]
        )

        diff = deepseek_review.get_diff(pr).text

        self.assertIn("server/internal/world/chunk.go", diff)
        # The names appear in the exclusion announcement; what must never appear is their content,
        # or a `+++ b/` header that would invite the model to review them.
        self.assertNotIn("+++ b/server/internal/gen/fbs/Chunk.go", diff)
        self.assertNotIn("m" * 100, diff)
        self.assertNotIn("r" * 100, diff)

    def test_source_files_survive_bindings_that_would_have_exhausted_the_budget(self):
        # The regression itself: without the exclusion the generated file alone overruns the
        # budget and every file after it is truncated away.
        pr = _FakePR(
            [
                _FakeFile("server/internal/gen/fbs/World.go", _patch_of(_CAP + 10_000, "m")),
                _FakeFile("schemas/world.fbs", _patch_of(100, "p")),
                _FakeFile("server/internal/world/chunk.go", _patch_of(100, "s")),
                _FakeFile("client/src/net/session.rs", _patch_of(100, "f")),
            ]
        )

        diff = deepseek_review.get_diff(pr).text

        self.assertNotIn("DIFF TRUNCATED", diff)
        for path in (
            "schemas/world.fbs",
            "server/internal/world/chunk.go",
            "client/src/net/session.rs",
        ):
            self.assertIn(path, diff)

    def test_still_truncates_when_real_source_alone_exceeds_the_budget(self):
        # The exclusion must not be mistaken for "truncation is gone" — a genuinely huge source
        # change is still capped, and that is correct.
        pr = _FakePR(
            [
                _FakeFile("server/internal/a.go", _patch_of(_CAP // 2 + 10_000, "a")),
                _FakeFile("server/internal/b.go", _patch_of(_CAP // 2 + 10_000, "b")),
            ]
        )

        diff = deepseek_review.get_diff(pr).text

        self.assertIn("DIFF TRUNCATED", diff)

    def test_only_generated_paths_are_excluded(self):
        # Guards against a substring rule that would swallow unrelated paths.
        pr = _FakePR(
            [
                _FakeFile("server/internal/genetics/traits.go", _patch_of(50, "g")),
                _FakeFile("scripts/gen-changelog.sh", _patch_of(50, "h")),
                _FakeFile("client/src/net/gen/chunk_generated.rs", _patch_of(50, "j")),
            ]
        )

        diff = deepseek_review.get_diff(pr).text

        self.assertIn("+++ b/server/internal/genetics/traits.go", diff)
        self.assertIn("+++ b/scripts/gen-changelog.sh", diff)
        self.assertNotIn("+++ b/client/src/net/gen/chunk_generated.rs", diff)
        self.assertNotIn("j" * 50, diff)

    def test_lockfiles_never_reach_the_reviewer(self):
        # PR #15 measured the damage: client/Cargo.lock was 5264 of 8319 non-generated
        # lines — 63% of the diff the model was asked to read, for a resolved version
        # graph nobody reviews.
        pr = _FakePR(
            [
                _FakeFile("client/Cargo.lock", _patch_of(5000, "l")),
                _FakeFile("server/go.sum", _patch_of(500, "s")),
                _FakeFile("client/src/net/codec.rs", _patch_of(50, "c")),
            ]
        )

        diff = deepseek_review.get_diff(pr).text

        self.assertIn("+++ b/client/src/net/codec.rs", diff)
        self.assertNotIn("+++ b/client/Cargo.lock", diff)
        self.assertNotIn("+++ b/server/go.sum", diff)
        self.assertNotIn("l" * 100, diff)
        self.assertNotIn("s" * 100, diff)

    def test_manifests_are_reviewed_even_though_their_lockfiles_are_not(self):
        # The distinction that makes the exclusion safe: a new dependency appears in the
        # manifest, and that is exactly what a reviewer must see. Excluding the manifest
        # would hide the only reviewable half of a dependency change.
        pr = _FakePR(
            [
                _FakeFile("client/Cargo.toml", _patch_of(20, "t")),
                _FakeFile("server/go.mod", _patch_of(20, "m")),
                _FakeFile("client/Cargo.lock", _patch_of(2000, "l")),
            ]
        )

        diff = deepseek_review.get_diff(pr).text

        self.assertIn("+++ b/client/Cargo.toml", diff)
        self.assertIn("+++ b/server/go.mod", diff)
        self.assertNotIn("+++ b/client/Cargo.lock", diff)

    def test_lockfile_rule_matches_by_exact_basename(self):
        # Not a suffix match: a hand-written .lock elsewhere is someone's source file, and
        # dropping it would silently remove it from every review.
        pr = _FakePR(
            [
                _FakeFile("docs/design.lock", _patch_of(30, "d")),
                _FakeFile("scripts/cargo.lock.sh", _patch_of(30, "k")),
                _FakeFile("client/Cargo.lock", _patch_of(30, "l")),
            ]
        )

        diff = deepseek_review.get_diff(pr).text

        self.assertIn("+++ b/docs/design.lock", diff)
        self.assertIn("+++ b/scripts/cargo.lock.sh", diff)
        self.assertNotIn("+++ b/client/Cargo.lock", diff)

    def test_excluded_files_are_still_announced_by_name_and_status(self):
        # Withholding the *body* of a generated file is the point; withholding the *fact* that it
        # changed is not: a reviewer that cannot see a deletion happened reports the deletion as
        # missing — a false finding the filter itself would cause.
        pr = _FakePR(
            [
                _FakeFile("server/internal/gen/fbs/Chunk.go", _patch_of(500, "m"), status="removed"),
                _FakeFile("server/internal/world/chunk.go", _patch_of(50, "s")),
            ]
        )

        diff = deepseek_review.get_diff(pr).text

        self.assertIn("EXCLUDED from this diff", diff)
        self.assertIn("removed server/internal/gen/fbs/Chunk.go", diff)
        # The name is announced; the body still is not.
        self.assertNotIn("m" * 100, diff)

    def test_announcement_is_absent_when_nothing_was_excluded(self):
        pr = _FakePR([_FakeFile("server/internal/world/chunk.go", _patch_of(50, "s"))])

        self.assertNotIn("EXCLUDED from this diff", deepseek_review.get_diff(pr).text)

    def test_a_file_cut_mid_body_is_reported_as_partial_not_reviewed(self):
        # One file larger than half the budget forces the fallback branch, which cuts mid-body.
        # Its `+++ b/` header is inside the kept text, so a naive "did we see a header for it?"
        # check counts it as fully reviewed — the same silent-signal bug this change removes.
        pr = _FakePR([_FakeFile("server/internal/huge.go", _patch_of(_CAP * 2, "h"))])

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            diff = deepseek_review.get_diff(pr).text
        log = buffer.getvalue()

        self.assertIn("DIFF TRUNCATED", diff)
        self.assertIn("server/internal/huge.go", log)
        self.assertIn("PARTIAL", log)

    def test_a_file_cut_at_a_boundary_is_reported_as_dropped_not_partial(self):
        # The boundary branch ends exactly between files, so nothing was half-seen.
        pr = _FakePR(
            [
                _FakeFile("server/internal/a.go", _patch_of(_CAP // 2 + 10_000, "a")),
                _FakeFile("server/internal/b.go", _patch_of(_CAP // 2 + 10_000, "b")),
            ]
        )

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            deepseek_review.get_diff(pr)
        log = buffer.getvalue()

        self.assertIn("server/internal/b.go", log)
        self.assertNotIn("PARTIAL", log)

    def test_is_generated_path_classification(self):
        self.assertTrue(deepseek_review.is_generated_path("server/internal/gen/fbs/Chunk.go"))
        self.assertTrue(deepseek_review.is_generated_path("client/src/net/gen/mod.rs"))
        self.assertTrue(deepseek_review.is_generated_path("client/src/net/chunk_generated.rs"))
        # A top-level directory of that name would still be generated output.
        self.assertTrue(deepseek_review.is_generated_path("gen/anything.go"))
        self.assertFalse(deepseek_review.is_generated_path("server/internal/genetics/traits.go"))
        self.assertFalse(deepseek_review.is_generated_path("scripts/gen-changelog.sh"))
        self.assertFalse(deepseek_review.is_generated_path("client/src/ui/generated_ui.rs"))
        self.assertFalse(deepseek_review.is_generated_path("schemas/world.fbs"))


class _RecordingPR(_FakePR):
    """A PR that records what would have been posted to GitHub."""

    def __init__(self, files=None):
        super().__init__(files or [_FakeFile("server/internal/a.go", _patch_of(50, "a"))])
        self.title = "A pull request"
        self.body = "Its description"
        self.number = 464
        self.changed_files = len(self._files)
        self.posted = []

    def get_reviews(self):
        return []

    def create_review(self, **kwargs):
        self.posted.append(kwargs)


class ReportedDiffSizeTests(unittest.TestCase):
    """
    The line a human reads first when asking "did the review actually happen".

    `get_diff` returned a string until #34, then a 3-field tuple. Every consumer was
    updated to use `.text` except this log line, which kept calling `len()` on the
    tuple — so it printed "Diff: 3 chars" for every pull request that followed,
    regardless of size. Read as a measurement it looked like total failure, and that
    reading produced issue #43 and PR #44 against a partial-API-response that had
    never happened. A number nobody can act on is harmless; this one got acted on.
    """

    def setUp(self):
        self._real_call = deepseek_review.call_deepseek
        deepseek_review.call_deepseek = lambda *a, **k: '{"review_complete": true, "comments": []}'

    def tearDown(self):
        deepseek_review.call_deepseek = self._real_call

    def _reported_size(self, pr):
        with contextlib.redirect_stdout(io.StringIO()) as out:
            deepseek_review.mode_full_review(None, None, pr, "github-actions[bot]")
        match = re.search(r"Diff: (\d+) chars across", out.getvalue())
        self.assertIsNotNone(match, "the diff-size line must be logged")
        return int(match.group(1))

    def test_the_reported_size_is_the_size_of_what_the_model_was_sent(self):
        pr = _RecordingPR([_FakeFile("server/internal/world/chunk.go", _patch_of(4000, "s"))])
        expected = len(deepseek_review.get_diff(pr).text)

        self.assertGreater(expected, 4000, "fixture sanity: the diff must be substantial")
        self.assertEqual(expected, self._reported_size(pr))

    def test_the_reported_size_tracks_the_diff_rather_than_being_constant(self):
        # The specific way the old code was wrong: len() of a 3-field tuple is 3 for a
        # 12,000-character diff and 3 for a 40-character one. Two sizes, one assertion —
        # a value that ignores its input cannot satisfy both.
        small = _RecordingPR([_FakeFile("server/internal/a.go", _patch_of(40, "a"))])
        large = _RecordingPR([_FakeFile("server/internal/b.go", _patch_of(9000, "b"))])

        self.assertLess(self._reported_size(small), self._reported_size(large))

    def test_the_truncation_warning_still_measures_the_string_it_truncated(self):
        # Inside get_diff, `diff` is a plain string, so len() is right there today.
        # Nothing pinned that it stays one.
        pr = _FakePR([_FakeFile("server/internal/big.go", _patch_of(_CAP * 2, "b"))])

        with contextlib.redirect_stdout(io.StringIO()) as out:
            text = deepseek_review.get_diff(pr).text

        match = re.search(r"Diff truncated \((\d+) chars\)", out.getvalue())
        self.assertIsNotNone(match, "truncation must report a size")
        self.assertEqual(len(text), int(match.group(1)))


class _RejectingPR(_RecordingPR):
    def create_review(self, **kwargs):
        raise deepseek_review.GithubException("approval rejected")


class MeasureOnlyReviewTests(unittest.TestCase):
    def setUp(self):
        self._real_call = deepseek_review.call_deepseek

    def tearDown(self):
        deepseek_review.call_deepseek = self._real_call

    def test_real_response_is_parsed_without_posting_or_spending_a_round(self):
        deepseek_review.call_deepseek = lambda *args, **kwargs: (
            '{"review_complete": true, "comments": []}'
        )
        pr = _RecordingPR()

        with mock.patch.dict(
            deepseek_review.os.environ,
            {"MEASURE_ONLY": "true", "MAX_ROUNDS": "1"},
            clear=False,
        ):
            with contextlib.redirect_stdout(io.StringIO()) as output:
                deepseek_review.mode_full_review(None, None, pr, _BOT_USERNAME)

        self.assertEqual([], pr.posted)
        self.assertIn("MEASURE ONLY", output.getvalue())
        self.assertIn("parsed final review content successfully", output.getvalue())


class ReviewBodyMarkerTests(unittest.TestCase):
    """
    A review body is where findings hide from the merge gate.

    General comments live in the review body and create no review thread, so the
    unresolved-thread count cannot see them (clinic-deck #464 merged with three of
    them unread while every gate printed green). The gate reads findings
    structurally — a body that still says something once the markers come off —
    which puts one obligation on this script: the one body that is prose and yet
    reports nothing has to say so.
    """

    def setUp(self):
        self._real_call = deepseek_review.call_deepseek

    def tearDown(self):
        deepseek_review.call_deepseek = self._real_call

    def _run(self, raw):
        deepseek_review.call_deepseek = lambda *args, **kwargs: raw
        pr = _RecordingPR()
        with contextlib.redirect_stdout(io.StringIO()):
            deepseek_review.mode_full_review(None, None, pr, "github-actions[bot]")
        self.assertEqual(1, len(pr.posted), "expected exactly one review to be posted")
        return pr.posted[0]

    def test_an_unreadable_diff_is_a_failure_not_an_empty_review(self):
        # #31: the fetch failed with 403 during a GitHub incident, the script reported
        # "Empty diff — nothing to review", and the check went green with no review. A
        # read failure and an empty diff justify opposite actions, so they must not share
        # a code path.
        class _BrokenPR(_RecordingPR):
            def get_files(self):
                raise deepseek_review.GithubException(403, {"message": "Resource not accessible"}, {})

        deepseek_review.call_deepseek = lambda *args, **kwargs: '{"review_complete": true, "comments": []}'
        pr = _BrokenPR()

        with contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "could not be read"):
                deepseek_review.mode_full_review(None, None, pr, "github-actions[bot]")

        self.assertEqual([], pr.posted, "nothing may be posted when the diff was never read")

    def test_a_genuinely_empty_diff_is_still_benign(self):
        # The other half of the same distinction: a diff with nothing readable in it — a
        # binary-only change — has nothing to review, and must stay quiet rather than
        # becoming a failure.
        #
        # Note what does *not* belong here: a PR of only generated files is not empty,
        # because the exclusion announcement is itself diff text. That case reaches the
        # model, which sees the announcement and correctly reports nothing — writing this
        # test the other way is what showed the difference.
        pr = _RecordingPR([_FakeFile("assets/texture.png", None)])
        deepseek_review.call_deepseek = lambda *args, **kwargs: '{"review_complete": true, "comments": []}'

        with contextlib.redirect_stdout(io.StringIO()):
            deepseek_review.mode_full_review(None, None, pr, "github-actions[bot]")

        self.assertEqual([], pr.posted)

    def test_a_truncated_diff_cannot_produce_a_clean_verdict(self):
        # #32, measured on PR #30: the budget ran out after the client files, so the whole
        # server half went unread and the review said "no substantive issues found" — and
        # the frozen rule passed. A partial read has something to report by construction.
        pr = _RecordingPR([
            _FakeFile("client/src/aaa.rs", _patch_of(_CAP + 10_000, "c")),
            _FakeFile("server/internal/game/collide.go", _patch_of(400, "s")),
            _FakeFile("server/internal/game/player.go", _patch_of(400, "p")),
        ])
        deepseek_review.call_deepseek = lambda *args, **kwargs: '{"review_complete": true, "comments": []}'

        with contextlib.redirect_stdout(io.StringIO()):
            deepseek_review.mode_full_review(None, None, pr, "github-actions[bot]")

        self.assertEqual(1, len(pr.posted))
        posted = pr.posted[0]

        # Not clean: the marker that means "nothing to report" must be absent, or the
        # frozen rule would wave the pull request through.
        self.assertNotIn(deepseek_review.NO_FINDINGS_MARKER, posted["body"])
        # And it must say which files nobody looked at, by name.
        self.assertIn("This review is incomplete", posted["body"])
        self.assertIn("server/internal/game/collide.go", posted["body"])
        self.assertIn("server/internal/game/player.go", posted["body"])
        # Stamped, so the partial pass is counted as the round it was.
        self.assertIn(deepseek_review.FULL_REVIEW_MARKER, posted["body"])

    def test_the_truncation_notice_names_every_dropped_file(self):
        # The cap this replaces was twenty names plus "…and N more", which hid exactly what
        # the notice exists to expose: the budget fills in file order, so a systematically
        # skipped directory sits *after* the first twenty.
        files = [_FakeFile("aaa/big.rs", _patch_of(_CAP + 10_000, "c"))]
        files += [_FakeFile(f"server/internal/game/f{i:02d}.go", _patch_of(200, "s")) for i in range(30)]
        pr = _RecordingPR(files)
        deepseek_review.call_deepseek = lambda *args, **kwargs: '{"review_complete": true, "comments": []}'

        with contextlib.redirect_stdout(io.StringIO()):
            deepseek_review.mode_full_review(None, None, pr, "github-actions[bot]")

        body = pr.posted[0]["body"]
        for i in range(30):
            self.assertIn(f"server/internal/game/f{i:02d}.go", body,
                          "a dropped file past the first twenty was not named")
        self.assertNotIn("…and", body, "nothing should have been collapsed at this size")

    def test_a_reply_is_told_which_files_it_cannot_see(self):
        # #32's failure mode through the other door: a reply composed from a partial diff,
        # with nothing telling the model what is missing, is a conclusion beyond its evidence.
        files = [_FakeFile("aaa/big.rs", _patch_of(_CAP + 10_000, "c")),
                 _FakeFile("server/internal/game/collide.go", _patch_of(300, "s"))]
        fetched = deepseek_review.get_diff(_FakePR(files))

        self.assertIn("server/internal/game/collide.go", fetched.dropped)

        # The reply path appends the list to the diff it hands the model. Assert on the
        # mechanism the code uses rather than re-running the whole reply flow, which needs a
        # comment tree this fake does not have.
        source = Path(deepseek_review.__file__).read_text()
        self.assertIn("are NOT in this diff at all", source)
        self.assertIn("if fetched.dropped:", source)

    def test_a_withheld_patch_is_an_unreadable_diff_not_a_binary_file(self):
        # #43, observed three times during a GitHub outage: the files endpoint answered
        # with entries carrying no patch, so nothing raised, and a 636-line pull request
        # became a three-character diff that read as legitimate. GitHub reports 0 changed
        # lines for a genuine binary, so the two are separable.
        pr = _RecordingPR([_FakeFile("client/src/world/mod.rs", None, additions=369, deletions=12)])
        deepseek_review.call_deepseek = lambda *args, **kwargs: '{"review_complete": true, "comments": []}'

        with contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "could not be read"):
                deepseek_review.mode_full_review(None, None, pr, "github-actions[bot]")

        self.assertEqual([], pr.posted, "a review must not be published from a partial read")

    def test_a_real_binary_file_is_still_just_skipped(self):
        # The other side of the discriminator: a binary file reports no changed lines and
        # must keep being counted and announced rather than failing the run.
        files = [_FakeFile("assets/texture.png", None, additions=0, deletions=0),
                 _FakeFile("client/src/world/mod.rs", _patch_of(200, "m"))]

        with contextlib.redirect_stdout(io.StringIO()):
            diff = deepseek_review.get_diff(_FakePR(files))

        self.assertFalse(diff.unreadable)
        self.assertIn("+++ b/client/src/world/mod.rs", diff.text)

    def test_a_network_failure_is_reported_rather_than_raised_raw(self):
        # PyGithub raises requests' exceptions when urllib3 exhausts its retries, which is
        # what a 503 storm produces. That escaped as a traceback until #43.
        class _UnreachablePR(_FakePR):
            def get_files(self):
                raise deepseek_review.RequestException("too many 503 error responses")

        with contextlib.redirect_stdout(io.StringIO()) as out:
            diff = deepseek_review.get_diff(_UnreachablePR([]))

        self.assertTrue(diff.unreadable)
        self.assertIn("GitHub API unavailable", out.getvalue())

    def test_an_unreadable_round_count_is_not_zero(self):
        # 0 means "no rounds spent" and lets another review run, so a lookup that failed
        # during an outage could bypass the one-round cap indefinitely — each run blind to
        # the ones before it.
        class _BlindPR(_FakePR):
            def get_reviews(self):
                raise deepseek_review.RequestException("too many 503 error responses")

        with self.assertRaisesRegex(RuntimeError, "budget is already spent is unknown"):
            deepseek_review._count_bot_reviews(_BlindPR([]), "github-actions[bot]")

    def test_a_complete_diff_still_produces_a_clean_verdict(self):
        # The guard must not fire on every review: a diff that fits is still allowed to
        # come back clean, with no truncation notice anywhere in it.
        posted = self._run('{"review_complete": true, "comments": []}')

        self.assertTrue(posted["body"].startswith(deepseek_review.NO_FINDINGS_MARKER))
        self.assertNotIn("This review is incomplete", posted["body"])

    def test_the_prompt_requires_findings_to_be_anchored(self):
        # Pinned for the same reason the markers are: this instruction is the only thing
        # keeping findings out of the review body, and a body finding creates no thread
        # for anyone to resolve. Measured before it existed: PRs #9 and #13 received
        # anchored findings and closed themselves; #15 and #24 received body-only findings
        # and both stalled waiting on a label.
        source = Path(deepseek_review.__file__).read_text()

        self.assertIn("ANCHOR EVERY FINDING YOU CAN", source)
        self.assertIn("create no review thread", source)
        # The escape hatch must stay narrow and stay explained, or the model widens it.
        self.assertIn("genuinely belongs to no single", source)

    def test_no_verdict_shape_ever_attempts_an_approve(self):
        # The regression guard for #22. GitHub forbids Actions from approving pull
        # requests, so an APPROVE is not a stricter verdict — it is a failed job, and it
        # failed on the one kind of PR that deserved it least: the flawless one. The
        # `"APPROVE" if review_complete else "COMMENT"` ternary is the shape that must
        # never come back, and it would pass a test that only checked the clean case.
        shapes = {
            "clean": '{"review_complete": true, "comments": []}',
            "complete with an observation": (
                '{"review_complete": true, "comments": ['
                '{"path": null, "line": null, "body": "Worth a look."}]}'
            ),
            "findings": (
                '{"review_complete": false, "comments": ['
                '{"path": null, "line": null, "body": "This is a bug."}]}'
            ),
        }

        for name, response in shapes.items():
            with self.subTest(shape=name):
                self.assertEqual("COMMENT", self._run(response)["event"])

    def test_clean_verdict_declares_itself_free_of_findings(self):
        posted = self._run('{"review_complete": true, "comments": []}')

        # COMMENT, not APPROVE: GitHub forbids Actions from approving pull requests, so
        # the marker carries the meaning that the review state used to (#22).
        self.assertEqual("COMMENT", posted["event"])
        # Unstamped by the round marker on purpose — that marker is what the round
        # budget counts, and a clean pass must leave a later push reviewable.
        self.assertNotIn(deepseek_review.FULL_REVIEW_MARKER, posted["body"])
        # BEGINS with, not merely contains. The gate exempts a review only in this exact
        # shape, because a body that just quotes the marker — DeepSeek reviews this
        # repository, where the marker is a string in the diff — would otherwise wave
        # through however many real findings sat beside it.
        self.assertTrue(posted["body"].startswith(deepseek_review.NO_FINDINGS_MARKER))
        self.assertIn("no substantive issues found", posted["body"])

    def test_empty_incomplete_verdict_fails_instead_of_silently_posting_nothing(self):
        deepseek_review.call_deepseek = lambda *args, **kwargs: (
            '{"review_complete": false, "comments": []}'
        )
        pr = _RecordingPR()

        with self.assertRaisesRegex(RuntimeError, "no actionable review verdict"):
            deepseek_review.mode_full_review(None, None, pr, "github-actions[bot]")

        self.assertEqual([], pr.posted)

    def test_clean_verdict_fails_when_github_rejects_the_review(self):
        deepseek_review.call_deepseek = lambda *args, **kwargs: (
            '{"review_complete": true, "comments": []}'
        )

        with self.assertRaisesRegex(RuntimeError, "GitHub rejected the review that records it"):
            deepseek_review.mode_full_review(
                None, None, _RejectingPR(), "github-actions[bot]"
            )

    def test_a_complete_verdict_carrying_general_comments_is_not_marked_clean(self):
        # The clinic-deck #478 shape: the model sets review_complete=true and still returns a
        # general observation. It is real feedback, so it must NOT look like the clean verdict —
        # and since it is a full review with content, it is stamped and does spend the round.
        posted = self._run(
            '{"review_complete": true, "comments": ['
            '{"path": null, "line": null, "body": "The drift guard silently drops unparseable lines."}]}'
        )

        self.assertEqual("COMMENT", posted["event"])
        self.assertNotIn(deepseek_review.NO_FINDINGS_MARKER, posted["body"])
        self.assertIn(deepseek_review.FULL_REVIEW_MARKER, posted["body"])
        self.assertIn("General Comments", posted["body"])

    def test_an_inline_only_review_still_reads_as_body_less(self):
        # Its findings ARE threads, and threads are already counted. The round marker
        # renders as nothing, so the body has to strip back to empty.
        posted = self._run(
            '{"review_complete": false, "comments": ['
            '{"path": "server/internal/a.go", "line": 1, "body": "Off by one."}]}'
        )

        self.assertEqual("COMMENT", posted["event"])
        self.assertEqual(deepseek_review.FULL_REVIEW_MARKER, posted["body"])
        self.assertNotIn(deepseek_review.NO_FINDINGS_MARKER, posted["body"])

    def test_markers_are_invisible_and_distinct(self):
        self.assertNotEqual(deepseek_review.FULL_REVIEW_MARKER, deepseek_review.NO_FINDINGS_MARKER)
        for marker in (deepseek_review.FULL_REVIEW_MARKER, deepseek_review.NO_FINDINGS_MARKER):
            self.assertTrue(marker.startswith("<!--") and marker.endswith("-->"))

    def test_markers_match_the_shell_helper_that_reads_them(self):
        # Both constants are duplicated in scripts/gh-automation.sh, which is what
        # actually evaluates the merge gate. A marker changed on one side only is
        # invisible at runtime: round accounting or the findings count simply stops
        # matching, silently, on every PR.
        helper = (Path(__file__).parents[2] / "scripts" / "gh-automation.sh").read_text()

        self.assertIn(
            f'DEEPSEEK_FULL_REVIEW_MARKER="{deepseek_review.FULL_REVIEW_MARKER}"',
            helper,
        )
        self.assertIn(
            f'DEEPSEEK_NO_FINDINGS_MARKER="{deepseek_review.NO_FINDINGS_MARKER}"',
            helper,
        )


# ─────────── Mode A wants an object, Mode B wants prose (#57) ───────────


class _FakeDeepSeekAPI:
    """A completions endpoint that behaves the way the real one does about JSON mode.

    Two behaviours, both documented by the API, and both invisible to a mock that does
    nothing but record the kwargs it was handed — which is exactly how #57 survived the
    review that predicted it. PR #16: "the mocked client won't catch an unsupported API
    parameter combination… if DeepSeek V4's reasoning endpoint does not support JSON mode,
    every full review will hit the sys.exit(1) error path." That thread was resolved on the
    strength of seventeen green pull requests, which were never evidence that the call was
    legal — only that the word "json" kept turning up in the diff.

      1. `response_format={"type": "json_object"}` is REFUSED unless the prompt contains the
         word "json" in some form. That 400 is what killed the reply on PR #54 at 81e874f.
      2. When it is accepted, the answer comes back as an object — so prose asked for this
         way arrives wrapped, which is the `{"response": "…"}` still sitting in PR #34's
         review threads.
    """

    def __init__(self, content):
        self.content = content
        self.kwargs = None

    @property
    def chat(self):
        # The caller reaches for client.chat.completions.create(...); this fake is all three.
        return types.SimpleNamespace(completions=self)

    def create(self, **kwargs):
        self.kwargs = kwargs
        content = self.content
        if (kwargs.get("response_format") or {}).get("type") == "json_object":
            prompt = " ".join(m["content"] for m in kwargs["messages"]).lower()
            if "json" not in prompt:
                raise ValueError(
                    "Error code: 400 - Prompt must contain the word 'json' in some form to "
                    "use 'response_format' of type 'json_object'."
                )
            if not content.lstrip().startswith("{"):
                content = json.dumps({"response": content})
        message = types.SimpleNamespace(content=content, reasoning_content="")
        return types.SimpleNamespace(
            choices=[types.SimpleNamespace(message=message, finish_reason="stop")]
        )


class _FakeReviewComment:
    def __init__(self, comment_id, login, body, in_reply_to_id=None):
        self.id = comment_id
        self.user = types.SimpleNamespace(login=login)
        self.body = body
        self.in_reply_to_id = in_reply_to_id
        self.path = "server/internal/a.go"
        self.line = 12


class _ThreadedPR(_RecordingPR):
    """A PR carrying one bot review comment and the developer's reply to it.

    None of the fixture's own material — title, description, diff, bot comment — contains the
    word "json", so by default neither does the conversation as a whole. That is not an
    oversight: it is the condition under which Mode B used to 400, so the default reproduces
    PR #54 rather than approximating it. `dev_reply` is a parameter because the opposite
    condition — the word present by luck — is the one that used to succeed and post a wrapper.
    """

    NO_SUCH_WORD = "It runs once per chunk load, so the cost is amortised."

    def __init__(self, dev_reply=NO_SUCH_WORD):
        super().__init__()
        self.bot_comment = _FakeReviewComment(1, _BOT_USERNAME, "This lookup is O(n) per voxel.")
        self.dev_reply = _FakeReviewComment(2, "a-developer", dev_reply, in_reply_to_id=1)
        self.replies = []

    def get_review_comment(self, comment_id):
        for comment in (self.bot_comment, self.dev_reply):
            if comment.id == comment_id:
                return comment
        raise deepseek_review.GithubException(f"no comment {comment_id}")

    def get_review_comments(self):
        return [self.bot_comment, self.dev_reply]

    def create_review_comment_reply(self, comment_id, body):
        self.replies.append((comment_id, body))


class JsonModeBelongsToTheCallerTests(unittest.TestCase):
    """
    #57: `call_deepseek` carried one mode's contract and the other inherited it.

    Mode A parses an object, so it asks for one; the word "json" in its prompt is what makes
    that legal. Mode B answers a human in prose and must not ask at all — its own prompt
    carries no such word, so asking left the outcome to whatever the diff and the thread
    happened to say: a 400 when absent, a wrapper around the prose when present. Both
    directions are pinned here; flip either side and one of these fails.
    """

    def _run_mode_a(self):
        api = _FakeDeepSeekAPI('{"review_complete": true, "comments": []}')
        pr = _RecordingPR()
        with contextlib.redirect_stdout(io.StringIO()):
            deepseek_review.mode_full_review(api, None, pr, _BOT_USERNAME)
        return api, pr

    def _run_mode_b(self, prose, dev_reply=_ThreadedPR.NO_SUCH_WORD):
        api = _FakeDeepSeekAPI(prose)
        pr = _ThreadedPR(dev_reply)
        with contextlib.redirect_stdout(io.StringIO()):
            deepseek_review.mode_reply(
                api,
                None,
                pr,
                pr.dev_reply.body,
                pr.dev_reply.id,
                pr.dev_reply.user.login,
                _BOT_USERNAME,
            )
        return api, pr

    def test_mode_a_asks_for_a_json_object_and_its_prompt_earns_it(self):
        # Two assertions in one run: the request carries the contract, and the prompt still
        # contains the word that permits it — the fake refuses the call otherwise, so
        # reaching a posted review at all is the second half of the test.
        api, pr = self._run_mode_a()

        self.assertEqual({"type": "json_object"}, api.kwargs["response_format"])
        self.assertEqual(
            deepseek_review.DEEPSEEK_MAX_OUTPUT_TOKENS,
            api.kwargs["max_tokens"],
        )
        self.assertEqual(1, len(pr.posted), "the review must still be posted")

    def test_mode_b_asks_for_no_response_format_at_all(self):
        api, _ = self._run_mode_b("You are right — it is amortised, so no change is needed.")

        self.assertNotIn("response_format", api.kwargs)
        self.assertEqual(
            deepseek_review.DEEPSEEK_MAX_OUTPUT_TOKENS,
            api.kwargs["max_tokens"],
        )

    def test_mode_b_posts_prose_verbatim_whether_or_not_the_thread_says_the_word(self):
        # Whether "json" turns up in the conversation used to decide whether the developer
        # got an answer at all. It must decide nothing now — and the two branches failed
        # differently under the old behaviour, which is why both are here: absent, the call
        # 400s and no reply is posted (PR #54); present, the call succeeds and the reply is
        # posted wrapped (PR #34).
        prose = (
            "You are right — it runs once per chunk load, so the cost is amortised.\n\n"
            "```go\n// no change needed\n```"
        )
        threads = {
            "no such word anywhere": _ThreadedPR.NO_SUCH_WORD,
            "the developer happens to say json": "It only parses the json manifest once.",
        }

        for label, dev_reply in threads.items():
            with self.subTest(thread=label):
                _, pr = self._run_mode_b(prose, dev_reply=dev_reply)

                # Verbatim, not merely "contains": PR #34's replies contain their prose too,
                # inside a wrapper that the thread renders literally.
                self.assertEqual([(2, prose)], pr.replies)

    def test_json_mode_is_what_wrapped_the_replies_on_pr_34(self):
        # The other half of the fake's fidelity, and the second symptom in #57. When the word
        # IS present, JSON mode does not fail — it succeeds and hands back an object, which is
        # what both bot replies on PR #34 still are. Same helper, same prompts, one flag: the
        # flag is the whole difference between an answer and `{"response": "…"}`.
        api = _FakeDeepSeekAPI("Plain prose, and nobody wanted it wrapped.")

        with contextlib.redirect_stdout(io.StringIO()):
            wrapped = deepseek_review.call_deepseek(
                api, "reply in json", "the question", json_mode=True
            )
            plain = deepseek_review.call_deepseek(
                api, "reply in json", "the question", json_mode=False
            )

        self.assertEqual(
            {"response": "Plain prose, and nobody wanted it wrapped."}, json.loads(wrapped)
        )
        self.assertEqual("Plain prose, and nobody wanted it wrapped.", plain)

    def test_the_fake_api_really_does_refuse_json_mode_without_the_word(self):
        # Non-vacuity for the two above. Without this rule the fixture cannot tell a legal
        # call from the one that 400'd on PR #54, and Mode B's tests would pass with JSON
        # mode switched back on.
        api = _FakeDeepSeekAPI("prose")

        with contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(SystemExit):
                deepseek_review.call_deepseek(api, "no such word", "nor here", json_mode=True)

    def test_the_log_says_which_contract_the_call_used(self):
        # This bug was only ever visible in the Actions log, so #46's rule applies to the
        # line that now names the contract: a diagnostic someone makes a decision from is an
        # output, and outputs are pinned here. "(prose)" against a full review, or its
        # absence against a reply, is a wrong call spotted without re-deriving anything.
        with contextlib.redirect_stdout(io.StringIO()) as prose_log:
            deepseek_review.call_deepseek(
                _FakeDeepSeekAPI("an answer"), "reply in json", "the question", json_mode=False
            )
        with contextlib.redirect_stdout(io.StringIO()) as json_log:
            deepseek_review.call_deepseek(
                _FakeDeepSeekAPI('{"review_complete": true}'), "in json", "the diff", json_mode=True
            )

        self.assertIn("(prose)", prose_log.getvalue())
        self.assertNotIn("(prose)", json_log.getvalue())

    def test_the_helper_has_no_default_contract_left_to_inherit(self):
        # The bug was never that JSON mode was wrong. It was that it was a default nobody
        # chose, in a helper shared by a mode that wanted it and a mode that did not. Either
        # default reintroduces that, so the parameter stays required and keyword-only.
        parameter = inspect.signature(deepseek_review.call_deepseek).parameters["json_mode"]

        self.assertIs(inspect.Parameter.empty, parameter.default)
        self.assertIs(inspect.Parameter.KEYWORD_ONLY, parameter.kind)


if __name__ == "__main__":
    unittest.main()
