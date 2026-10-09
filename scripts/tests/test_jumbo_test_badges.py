"""Offline producer tests, including real Git fast-forward races."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("badges", Path(__file__).parents[1] / "jumbo_test_badges.py")
badges = importlib.util.module_from_spec(spec)
spec.loader.exec_module(badges)
SHA = "a" * 40
SOURCE = {"repository": "zephytiju/JuntaiFuseAPI", "workflow": "jumbo-publish.yml"}


def fixtures(run_id=20, attempt=1):
    run = {"id": run_id, "run_attempt": attempt, "event": "push", "head_branch": "main",
           "status": "completed", "head_sha": "b" * 40,
           "head_repository": {"full_name": SOURCE["repository"]},
           "path": ".github/workflows/jumbo-publish.yml",
           "referenced_workflows": [{"path": "zephytiju/JumboBuild/.github/workflows/jumbo-verify.yml@" + SHA,
                                      "sha": SHA}]}
    job = {"name": "verify / verify", "conclusion": "success", "completed_at": "2026-10-08T23:00:00Z",
           "steps": [{"name": "Jumbo tests (python)", "conclusion": "success",
                      "started_at": "2026-10-08T22:59:00Z", "completed_at": "2026-10-08T23:00:00Z"},
                     {"name": "Jumbo tests (npm)", "conclusion": "skipped"}]}
    return run, [job]


def result(run_id=20, attempt=1):
    run, jobs = fixtures(run_id, attempt)
    return badges.result_from_run(SOURCE, "main", run, jobs, SHA)


class Outcomes(unittest.TestCase):
    def test_real_test_outcomes_and_early_infrastructure_failure(self):
        for job_conclusion, step_conclusion, expected in [
            ("success", "success", "passed"), ("failure", "failure", "failed"),
            ("failure", "skipped", "error"), ("cancelled", "cancelled", "error"),
            ("success", "skipped", "not-run"), ("failure", "success", "error"),
        ]:
            with self.subTest(expected=expected):
                run, jobs = fixtures()
                jobs[0]["conclusion"] = job_conclusion
                jobs[0]["steps"][0]["conclusion"] = step_conclusion
                got = badges.result_from_run(SOURCE, "main", run, jobs, SHA)
                self.assertEqual(got["outcome"], expected)
                self.assertEqual(got["commit"], run["head_sha"])
                self.assertEqual(got["completedAt"], jobs[0]["completed_at"])
                self.assertNotIn("coverage", got)

    def test_unapproved_workflow_or_missing_measurements_never_pass(self):
        for mutation in ("legacy", "missing", "duplicate"):
            run, jobs = fixtures()
            if mutation == "legacy":
                run["referenced_workflows"] = []
            elif mutation == "missing":
                jobs = [dict(jobs[0], name="other")]
            else:
                jobs += copy.deepcopy(jobs)
            self.assertEqual(badges.result_from_run(SOURCE, "main", run, jobs, SHA)["outcome"], "unknown")

    def test_untrusted_runs_are_rejected(self):
        for key, value in [("event", "pull_request"), ("event", "workflow_dispatch"),
                           ("head_branch", "feature"), ("status", "in_progress"),
                           ("path", ".github/workflows/other.yml"),
                           ("head_repository", {"full_name": "attacker/fork"})]:
            run, jobs = fixtures()
            run[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                badges.result_from_run(SOURCE, "main", run, jobs, SHA)

    def test_startup_failure_with_no_jobs_replaces_a_passing_badge(self):
        run, _ = fixtures(run_id=21)
        run.update(conclusion="startup_failure", updated_at="2026-10-08T23:01:00Z")
        got = badges.result_from_run(SOURCE, "main", run, [], SHA)
        self.assertEqual(got["outcome"], "error")
        self.assertEqual(got["completedAtSource"], "run.updated_at")
        self.assertEqual(got["measurements"], [])
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            badges.write_result(root, result())
            self.assertTrue(badges.write_result(root, got))
            self.assertIn("infrastructure error", (root / "badges/zephytiju/JuntaiFuseAPI/main/tests.svg").read_text())

    def test_collector_uses_default_branch_push_and_exact_attempt(self):
        run, jobs = fixtures(attempt=2)
        calls = []
        def api(path):
            calls.append(path)
            if path == "repos/" + SOURCE["repository"]:
                return {"default_branch": "main"}
            if "/jobs?" in path:
                return {"jobs": jobs}
            return {"workflow_runs": [run]}
        with patch.object(badges, "api", side_effect=api):
            got = badges.collect(SOURCE, SHA)
        self.assertEqual(got["runAttempt"], 2)
        self.assertIn("event=push&branch=main&status=completed", calls[1])
        self.assertIn("/attempts/2/jobs?", calls[2])

    def test_jobs_are_paginated(self):
        with patch.object(badges, "api", side_effect=[{"jobs": [{}] * 100}, {"jobs": [{}]}]):
            self.assertEqual(len(badges.pages("repos/a/b/jobs", "jobs")), 101)


class Rendering(unittest.TestCase):
    def test_svg_is_standalone_xml_and_escapes_both_contexts(self):
        xml = badges.svg('tests <&"', 'value <&"', '#555')
        root = ET.fromstring(xml)
        self.assertEqual(root.attrib["aria-label"], 'tests <&": value <&"')
        self.assertNotIn("<script", xml)

    def test_branch_encoding_metadata_and_monotonic_attempts(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            got = result()
            got["branch"] = "release/a&b"
            self.assertTrue(badges.write_result(root, got))
            record = root / "badges/zephytiju/JuntaiFuseAPI/release%2Fa%26b/status.json"
            self.assertEqual(json.loads(record.read_text()), got)
            self.assertFalse(badges.write_result(root, got))
            older = dict(got, runId=19, runAttempt=99)
            self.assertFalse(badges.write_result(root, older))
            self.assertTrue(badges.write_result(root, dict(got, runAttempt=2)))
            self.assertEqual({p.name for p in record.parent.iterdir()}, {"status.json", "tests.svg", "last-run.svg"})

    def test_path_traversal_and_symlinks_refused(self):
        with self.assertRaises(ValueError):
            badges.component("..")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "badges").symlink_to(root)
            with self.assertRaises(ValueError):
                badges.write_result(root, result())
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            badges.write_result(root, result())
            dest = root / "badges/zephytiju/JuntaiFuseAPI/main/tests.svg"
            dest.unlink()
            dest.symlink_to(root / "outside")
            with self.assertRaises(ValueError):
                badges.write_result(root, result(21))


class Publication(unittest.TestCase):
    def git(self, path, *args):
        return subprocess.check_output(["git", "-C", str(path), *args], text=True, stderr=subprocess.DEVNULL).strip()

    def test_fast_forward_race_rechecks_order_and_preserves_package_history(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            remote, checkout, racer = root / "remote.git", root / "writer", root / "racer"
            subprocess.check_call(["git", "init", "--bare", "--initial-branch=main", str(remote)], stdout=subprocess.DEVNULL)
            subprocess.check_call(["git", "clone", str(remote), str(checkout)], stderr=subprocess.DEVNULL)
            for key, value in [("user.name", "Badge Test"), ("user.email", "badge@example.invalid")]:
                self.git(checkout, "config", key, value)
            (checkout / "index").mkdir()
            package = checkout / "index/package.jsonl"
            package.write_text('{"immutable":true}\n')
            self.git(checkout, "add", ".")
            self.git(checkout, "commit", "-m", "seed")
            self.git(checkout, "push", "origin", "main")
            subprocess.check_call(["git", "clone", str(remote), str(racer)], stderr=subprocess.DEVNULL)
            for key, value in [("user.name", "Badge Test"), ("user.email", "badge@example.invalid")]:
                self.git(racer, "config", key, value)
            original_run = subprocess.run
            raced = False
            def push_race(args, **kwargs):
                nonlocal raced
                if args[:3] == ["git", "push", "origin"] and not raced:
                    raced = True
                    badges.write_result(racer, result(30))
                    self.git(racer, "add", "badges")
                    self.git(racer, "commit", "-m", "newer result")
                    self.git(racer, "push", "origin", "main")
                return original_run(args, **kwargs)
            head = self.git(checkout, "rev-parse", "HEAD")
            with patch.object(badges.subprocess, "run", side_effect=push_race):
                self.assertFalse(badges.publish(checkout, [result(20)]))
            self.assertTrue(raced)
            self.assertEqual(self.git(checkout, "rev-parse", "HEAD"), head)
            self.assertEqual(package.read_text(), '{"immutable":true}\n')
            self.assertTrue(badges.publish(checkout, [result(31)]))
            self.git(racer, "pull", "--ff-only")
            self.assertEqual(json.loads((racer / "badges/zephytiju/JuntaiFuseAPI/main/status.json").read_text())["runId"], 31)
            self.assertEqual(self.git(racer, "log", "--oneline", "--", "index/"), self.git(checkout, "log", "--oneline", "--", "index/"))
            (checkout / "dirty").write_text("pending")
            with self.assertRaises(ValueError):
                badges.publish(checkout, [result(32)])


if __name__ == "__main__":
    unittest.main()
