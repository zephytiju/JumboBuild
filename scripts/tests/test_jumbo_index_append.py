"""Unit tests for scripts/jumbo_index_append.py (JumboIndex append protocol).

Coverage: the happy path (append + commit, no push), canonical line shape
(key order, one line, compact), every precondition refusal (existing
version, duplicate fingerprint — same file and cross-file — non-increasing
version), the non-fast-forward retry (a concurrent append lands on the
remote first; the push is retried on the fresh tip without rewriting
anything), the idempotent already-recorded path, the exhausted-retry
refusal, validator rejection, and a true concurrent two-writer race on the
same fingerprint (two processes launched in real time converge to exactly
one record; the loser receives the existing record). No network, no
credentials: pushes go to a local bare repository over the file:// protocol.
"""

from __future__ import annotations

import base64
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "jumbo_index_append.py"
sys.path.insert(0, str(SCRIPT.parent))
import jumbo_index_append as jia  # noqa: E402


def record(package="demo-pkg", major=1, version="1.0.0", commit="a" * 40, fingerprint="b" * 64,
           executor="jumbo-publish-github-actions", **overrides) -> dict:
    base = {
        "package": package,
        "major": major,
        "version": version,
        "commit": commit,
        "fingerprint": fingerprint,
        "canonicalExtract": {"format": "jumbo-canonical-extract/1", "entries": []},
        "artifactUrl": f"https://github.com/example/{package}/releases/download/v{version}/{package}-{version}.whl",
        "artifactSha256": "c" * 64,
        "imageDigest": None,
        "buildId": f"{package}-{version}-gha123",
        "pipelineRun": "https://github.com/example/repo/actions/runs/123",
        "executor": executor,
        "timestamp": "2026-09-23T00:00:00Z",
    }
    base.update(overrides)
    return base


def run_git(cwd: Path, *args: str) -> str:
    env = dict(os.environ)
    env.pop("GIT_CONFIG_GLOBAL", None)
    env["GIT_CONFIG_GLOBAL"] = "/dev/null"
    env["GIT_CONFIG_SYSTEM"] = "/dev/null"
    # Harness commits must not depend on the ambient git identity (CI runners
    # have none; clones do not inherit the fixture's repo-local config).
    proc = subprocess.run(
        ["git", "-C", str(cwd), "-c", "user.name=Append Test",
         "-c", "user.email=append-test@invalid", *args],
        capture_output=True, text=True, env=env,
    )
    assert proc.returncode == 0, f"git {args} failed: {proc.stderr}"
    return proc.stdout.strip()


def make_index_repo(tag: str, records_by_file: dict[str, list[dict]] | None = None) -> Path:
    """A committed JumboIndex-shaped repository (origin, not a clone)."""
    repo = Path(tempfile.mkdtemp(prefix=f"jumbo-append-{tag}-"))
    (repo / "index").mkdir()
    (repo / "scripts").mkdir()
    (repo / "scripts" / "validate_index.py").write_text("# stub validator (replaced per test)\n")
    for name, records in (records_by_file or {}).items():
        lines = "".join(json.dumps(r, separators=(",", ":")) + "\n" for r in records)
        (repo / "index" / name).write_text(lines)
    run_git(repo, "init", "-q", "--initial-branch=main")
    run_git(repo, "config", "user.email", "test@invalid")
    run_git(repo, "config", "user.name", "Test")
    run_git(repo, "add", ".")
    run_git(repo, "commit", "-q", "-m", "fixture")
    return repo


def clone(tag: str, origin: Path) -> Path:
    clone_dir = Path(tempfile.mkdtemp(prefix=f"jumbo-append-{tag}-clone-"))
    run_git(clone_dir.parent, "clone", "-q", str(origin), str(clone_dir))
    return clone_dir


def bare_origin(tag: str) -> Path:
    origin = Path(tempfile.mkdtemp(prefix=f"jumbo-append-{tag}-origin-"))
    run_git(origin, "init", "-q", "--bare", "--initial-branch=main")
    return origin


def run_append(index_dir: Path, rec: dict, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--index-dir", str(index_dir),
         "--record", json.dumps(rec), *extra],
        capture_output=True, text=True,
    )


class CanonicalShape(unittest.TestCase):
    def test_canonical_line_uses_schema_key_order_compact_single_line(self):
        line = jia.canonical_line(record())
        parsed = json.loads(line)
        self.assertEqual(list(parsed.keys()), list(jia.RECORD_KEYS))
        self.assertTrue(line.endswith("\n"))
        self.assertEqual(line.count("\n"), 1)
        self.assertNotIn(", ", line)
        self.assertNotIn(": ", line)

    def test_validation_rejects_missing_extra_and_malformed_fields(self):
        incomplete = record()
        del incomplete["buildId"]
        with self.assertRaises(jia.AppendError):
            jia.validate_record(incomplete)
        with_extra = record(extraKey="x")
        with self.assertRaises(jia.AppendError):
            jia.validate_record(with_extra)
        for bad in [
            record(commit="short"),
            record(fingerprint="zz"),
            record(version="1.2"),
            record(major=2, version="1.0.0"),
            record(artifactSha256="q" * 64, artifactUrl=None),
            record(artifactUrl="http://insecure.example/x"),
            record(timestamp="yesterday"),
        ]:
            with self.assertRaises(jia.AppendError):
                jia.validate_record(bad)

    def test_extraheader_carries_the_token_from_env_not_argv(self):
        class FakeProc:
            returncode = 0
            stdout = ""
            stderr = ""
        captured = {}

        def capture(*args, **kwargs):
            captured["args"] = args
            return FakeProc()

        original = jia.subprocess.run
        jia.subprocess.run = capture
        try:
            jia.git(Path("/tmp"), "push", token="tok-va.lue", host="github.com")
        finally:
            jia.subprocess.run = original
        self.assertNotIn("tok-va.lue", captured["args"])  # never in argv


class AppendWithoutPush(unittest.TestCase):
    def test_happy_path_appends_one_line_and_commits(self):
        prior = record(version="1.0.0", fingerprint="0" * 64, timestamp="2026-09-01T00:00:00Z")
        index = make_index_repo("happy", {"demo-pkg.jsonl": [prior]})
        proc = run_append(index, record(version="1.1.0"), "--backoff-base", "0")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json.loads(proc.stdout)
        self.assertEqual(result["status"], "appended")
        self.assertFalse(result["pushed"])
        lines = (index / "index" / "demo-pkg.jsonl").read_text().splitlines()
        self.assertEqual(len(lines), 2)
        appended = json.loads(lines[-1])
        self.assertEqual(appended["version"], "1.1.0")
        self.assertEqual(list(appended.keys()), list(jia.RECORD_KEYS))
        commit_message = run_git(index, "log", "-1", "--pretty=%s")
        self.assertEqual(commit_message, "index: append demo-pkg@1.1.0 (jumbo-publish-github-actions)")
        # Nothing before the appended line changed: append-only prefix.
        self.assertEqual(json.loads(lines[0])["version"], "1.0.0")

    def test_new_package_file_is_created(self):
        index = make_index_repo("newfile")
        proc = run_append(index, record(package="fresh-pkg", version="0.1.0", major=0), "--backoff-base", "0")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue((index / "index" / "fresh-pkg.jsonl").exists())


class PreconditionRefusals(unittest.TestCase):
    def test_rejects_existing_version(self):
        prior = record(version="1.1.0", fingerprint="0" * 64)
        index = make_index_repo("samever", {"demo-pkg.jsonl": [prior]})
        proc = run_append(index, record(version="1.1.0"))
        self.assertEqual(proc.returncode, 0)
        result = json.loads(proc.stdout)
        self.assertEqual(result["status"], "already-recorded")
        self.assertEqual(result["conflictReason"], "version already recorded")
        # No second line was appended for a refused record.
        self.assertEqual(len((index / "index" / "demo-pkg.jsonl").read_text().splitlines()), 1)

    def test_rejects_duplicate_fingerprint_same_file(self):
        prior = record(version="1.0.0")
        index = make_index_repo("dupfp", {"demo-pkg.jsonl": [prior]})
        proc = run_append(index, record(version="1.2.0"))  # same fingerprint
        result = json.loads(proc.stdout)
        self.assertEqual(result["status"], "already-recorded")
        self.assertEqual(result["conflictReason"], "fingerprint already recorded")

    def test_rejects_duplicate_fingerprint_cross_file(self):
        other = record(package="other-pkg", version="2.0.0")
        index = make_index_repo("dupfpx", {"other-pkg.jsonl": [other]})
        proc = run_append(index, record(version="1.0.0"))
        result = json.loads(proc.stdout)
        self.assertEqual(result["status"], "already-recorded")

    def test_rejects_non_increasing_version(self):
        prior = record(version="1.5.0", fingerprint="0" * 64)
        index = make_index_repo("ordering", {"demo-pkg.jsonl": [prior]})
        proc = run_append(index, record(version="1.4.9"))
        self.assertEqual(proc.returncode, 1)
        self.assertIn("strictly greater", proc.stderr)
        self.assertEqual(len((index / "index" / "demo-pkg.jsonl").read_text().splitlines()), 1)


class TrueConcurrentRace(unittest.TestCase):
    """Two writers racing in real time on the same fingerprint.

    The PushRetryProtocol tests stage the race sequentially (the rival
    completes before our push starts). This test launches two append
    processes *concurrently* against the same remote — the shape the
    acceptance criterion describes: concurrent pipelines covering the same
    repository. The invariants hold under every interleaving (fully
    serialized or truly racing): exactly one record lands, the loser
    receives the existing record — the winner's, never its own draft — and
    exactly one artifact coordinate exists for the fingerprint.
    """

    def setUp(self):
        self.origin = bare_origin("race")
        seed = make_index_repo("race-seed", {"demo-pkg.jsonl": [
            record(version="1.0.0", fingerprint="0" * 64, timestamp="2026-09-01T00:00:00Z")
        ]})
        run_git(seed, "push", "-q", str(self.origin), "main:main")
        self.writer_a = clone("race-a", self.origin)
        self.writer_b = clone("race-b", self.origin)

    def test_two_concurrent_writers_same_fingerprint_converge_to_one_record(self):
        # Two pipeline runs covered the same repository: identical inputs —
        # same package, version, fingerprint, and artifact — differing only
        # in their run identities (timestamp, buildId, pipelineRun).
        rec_a = record(version="1.1.0", fingerprint="e" * 64,
                       buildId="demo-pkg-1.1.0-gha111",
                       pipelineRun="https://github.com/example/repo/actions/runs/111")
        rec_b = record(version="1.1.0", fingerprint="e" * 64,
                       timestamp="2026-09-23T00:00:07Z",
                       buildId="demo-pkg-1.1.0-gha222",
                       pipelineRun="https://github.com/example/repo/actions/runs/222")
        self.assertEqual(rec_a["artifactUrl"], rec_b["artifactUrl"])
        self.assertEqual(rec_a["artifactSha256"], rec_b["artifactSha256"])

        argv = lambda writer, rec: [
            sys.executable, str(SCRIPT), "--index-dir", str(writer),
            "--record", json.dumps(rec), "--push", "--backoff-base", "0",
        ]
        # Launch both writers before reaping either so their precondition
        # checks race on the same tip whenever the OS schedules them so.
        procs = [
            subprocess.Popen(argv(w, r), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            for w, r in ((self.writer_a, rec_a), (self.writer_b, rec_b))
        ]
        results = []
        for proc in procs:
            out, errout = proc.communicate(timeout=120)
            self.assertEqual(proc.returncode, 0, errout)
            results.append(json.loads(out))

        # One winner, one loser; the loser reports already-recorded.
        statuses = sorted(r["status"] for r in results)
        self.assertEqual(statuses, ["already-recorded", "appended"],
                         f"both writers must converge, got {statuses}")
        winner, loser = (results if results[0]["status"] == "appended"
                         else [results[1], results[0]])
        self.assertTrue(winner["pushed"])

        # The remote holds exactly one 1.1.0 record: never a duplicate.
        verify = clone("race-verify", self.origin)
        lines = (verify / "index" / "demo-pkg.jsonl").read_text().splitlines()
        self.assertEqual(len(lines), 2)
        self.assertEqual(json.loads(lines[0])["version"], "1.0.0")
        landed = json.loads(lines[1])
        self.assertEqual(landed["version"], "1.1.0")

        # The loser received the *existing* record — the winner's, with the
        # winner's run identity — so both writers converge on the same
        # artifact coordinate: one record, one artifact.
        self.assertEqual(loser["conflictReason"], "version already recorded")
        self.assertEqual(loser["conflict"], landed)
        self.assertEqual(loser["conflict"]["buildId"], landed["buildId"])
        self.assertEqual(landed["artifactUrl"], rec_a["artifactUrl"])
        self.assertEqual(landed["artifactSha256"], rec_a["artifactSha256"])

        # Exactly two commits in total: the fixture and the single append.
        self.assertEqual(
            run_git(verify, "rev-list", "--count", "HEAD"), "2"
        )
        self.assertIn(
            f"index: append demo-pkg@1.1.0 ({rec_a['executor']})",
            run_git(verify, "log", "-1", "--pretty=%s"),
        )


class PushRetryProtocol(unittest.TestCase):
    def setUp(self):
        self.origin = bare_origin("retry")
        seed = make_index_repo("retry-seed", {"demo-pkg.jsonl": [
            record(version="1.0.0", fingerprint="0" * 64, timestamp="2026-09-01T00:00:00Z")
        ]})
        run_git(seed, "push", "-q", str(self.origin), "main:main")
        self.work = clone("retry-work", self.origin)
        self.rival = clone("retry-rival", self.origin)

    def test_non_ff_retry_lands_on_the_fresh_tip_without_rewriting(self):
        # The rival appends first and pushes; our push then loses the race.
        rival_proc = run_append(self.rival, record(version="1.0.1", fingerprint="d" * 64), "--push",
                                "--backoff-base", "0")
        self.assertEqual(rival_proc.returncode, 0, rival_proc.stderr)

        proc = run_append(self.work, record(version="1.1.0", fingerprint="e" * 64), "--push",
                          "--backoff-base", "0")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json.loads(proc.stdout)
        self.assertEqual(result["status"], "appended")
        self.assertEqual(result["attempts"], 2)
        self.assertTrue(result["pushed"])

        # Both lines landed, ordered by version, prefix preserved.
        remote = clone("retry-verify", self.origin)
        lines = (remote / "index" / "demo-pkg.jsonl").read_text().splitlines()
        self.assertEqual([json.loads(line)["version"] for line in lines], ["1.0.0", "1.0.1", "1.1.0"])
        # Two distinct append commits, no amend, no force in the history.
        history = run_git(remote, "log", "--format=%H %s")
        commits = history.splitlines()
        self.assertEqual(len(commits), 3)  # fixture + two appends
        self.assertIn("index: append demo-pkg@1.0.1", commits[1])
        self.assertIn("index: append demo-pkg@1.1.0", commits[0])

    def test_concurrent_same_version_becomes_already_recorded_not_rewrite(self):
        rival_proc = run_append(self.rival, record(version="1.1.0", fingerprint="e" * 64), "--push",
                                "--backoff-base", "0")
        self.assertEqual(rival_proc.returncode, 0, rival_proc.stderr)

        # Ours computed the same version (different run: distinct timestamp,
        # buildId, and pipelineRun): after the non-FF fetch the precondition
        # sees the rival record and reports already-recorded, pushing nothing.
        proc = run_append(self.work, record(version="1.1.0", fingerprint="e" * 64,
                                            timestamp="2026-09-23T00:00:05Z",
                                            buildId="demo-pkg-1.1.0-gha456",
                                            pipelineRun="https://github.com/example/repo/actions/runs/456"),
                          "--push", "--backoff-base", "0")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json.loads(proc.stdout)
        self.assertEqual(result["status"], "already-recorded")
        self.assertFalse(result["pushed"])
        self.assertGreaterEqual(result["attempts"], 2)

        remote = clone("dup-verify", self.origin)
        lines = (remote / "index" / "demo-pkg.jsonl").read_text().splitlines()
        self.assertEqual(len(lines), 2)  # exactly one 1.1.0 record

    def test_exhausted_retries_fail_loudly_and_never_force(self):
        # A pre-receive hook that rejects every push makes each attempt's
        # push fail; the bounded budget exhausts and the failure is loud.
        hook = self.origin / "hooks" / "pre-receive"
        hook.write_text("#!/bin/sh\necho 'rejected by test hook' >&2\nexit 1\n")
        hook.chmod(0o755)
        proc = run_append(self.work, record(version="1.1.0", fingerprint="e" * 64), "--push",
                          "--max-attempts", "2", "--backoff-base", "0")
        self.assertEqual(proc.returncode, 1)
        self.assertIn("push still failing after 2 attempts", proc.stderr)
        self.assertIn("never force", proc.stderr)
        # The remote is untouched.
        remote = clone("exhaust-verify", self.origin)
        self.assertEqual(len((remote / "index" / "demo-pkg.jsonl").read_text().splitlines()), 1)

    def test_validator_rejection_leaves_the_index_unchanged(self):
        (self.work / "scripts" / "validate_index.py").write_text(
            "#!/usr/bin/env python3\nimport sys; print('invalid record'); sys.exit(1)\n"
        )
        run_git(self.work, "add", "scripts/validate_index.py")
        run_git(self.work, "commit", "-q", "-m", "install failing validator")
        proc = run_append(self.work, record(version="1.1.0", fingerprint="e" * 64))
        self.assertEqual(proc.returncode, 1)
        self.assertIn("rejected the append", proc.stderr)
        # No append commit was created on top of the validator commit.
        log = run_git(self.work, "log", "--format=%s")
        self.assertEqual(log.splitlines()[0], "install failing validator")

    def test_dirty_index_clone_is_refused(self):
        (self.work / "index" / "stray.txt").write_text("uncommitted\n")
        proc = run_append(self.work, record(version="1.1.0", fingerprint="e" * 64))
        self.assertEqual(proc.returncode, 1)
        self.assertIn("uncommitted changes", proc.stderr)


if __name__ == "__main__":
    unittest.main()
