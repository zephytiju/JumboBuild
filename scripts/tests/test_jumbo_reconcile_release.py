"""Offline historical release recovery; no release/index/network mutation."""

import hashlib
import io
import json
import sys
import tarfile
import unittest
import zipfile
from copy import deepcopy
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from jumbo_reconcile_release import (  # noqa: E402
    RecoveryError, make_record, primary_asset, select_release, verify_package,
)


def wheel(name="example-package", version="0.7.4"):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("example_package-0.7.4.dist-info/METADATA", f"Name: {name}\nVersion: {version}\n")
    return buffer.getvalue()


class HistoricalReleaseTests(unittest.TestCase):
    def setUp(self):
        self.data = wheel()
        self.asset = {
            "name": "example_package-0.7.4-py3-none-any.whl", "id": 12,
            "size": len(self.data), "digest": "sha256:" + hashlib.sha256(self.data).hexdigest(),
            "browser_download_url": "https://github.com/org/repo/releases/download/v0.7.4/example_package-0.7.4-py3-none-any.whl",
        }
        self.release = {
            "tag_name": "v0.7.4", "immutable": True, "draft": False,
            "prerelease": False, "body": "Historical native release", "assets": [self.asset],
        }
        self.records = [{"package": "example-package", "version": "0.6.0"}]

    def record(self, **kwargs):
        args = dict(repository="org/repo", package="example-package", major=0, ecosystem="python", commit="a" * 40)
        args.update(kwargs)
        return make_record(self.release, self.asset, self.data, **args)

    def test_imports_exact_historical_bytes_without_inventing_build(self):
        selected = select_release([self.release], self.records, "example-package", 0)
        self.assertEqual(primary_asset(selected, "example-package", "python"), self.asset)
        record = self.record()
        self.assertEqual(record["version"], "0.7.4")
        self.assertEqual(record["executor"], "bootstrap")
        self.assertEqual(record["artifactSha256"], hashlib.sha256(self.data).hexdigest())
        for field in ["fingerprint", "canonicalExtract", "buildId", "imageDigest", "pipelineRun"]:
            self.assertIsNone(record[field])

    def test_aligned_history_is_idempotent(self):
        self.records.append({"package": "example-package", "version": "0.7.4"})
        self.assertIsNone(select_release([self.release], self.records, "example-package", 0))

    def test_latest_stable_same_major_only(self):
        other = [dict(self.release, tag_name=tag) for tag in ["v0.7.0", "v0.8.0-rc.1", "v1.0.0"]]
        self.assertEqual(select_release(other + [self.release], self.records, "example-package", 0), self.release)

    def test_does_not_append_older_major_after_newer_major_history(self):
        self.records.append({"package": "example-package", "version": "1.0.0"})
        self.assertIsNone(select_release([self.release], self.records, "example-package", 0))

    def test_mutable_or_interrupted_jumbo_release_refused(self):
        for changes in [{"immutable": False}, {"body": "jumbo release of **example-package**"}]:
            with self.subTest(changes=changes), self.assertRaises(RecoveryError):
                select_release([dict(self.release, **changes)], self.records, "example-package", 0)

    def test_wrong_source_or_repository_refused(self):
        for changes in [{"repository": "other/repo"}, {"commit": "main"}, {"major": 1}]:
            with self.subTest(changes=changes), self.assertRaises(RecoveryError):
                self.record(**changes)

    def test_tampered_bytes_or_digest_refused(self):
        self.data += b"tampered"
        with self.assertRaises(RecoveryError):
            self.record()
        self.asset["size"] = len(self.data)
        with self.assertRaises(RecoveryError):
            self.record()

    def test_wrong_package_metadata_refused_even_with_matching_digest(self):
        for name, version in [("other", "0.7.4"), ("example-package", "0.7.3")]:
            self.data = wheel(name, version)
            self.asset.update(size=len(self.data), digest="sha256:" + hashlib.sha256(self.data).hexdigest())
            with self.subTest(name=name, version=version), self.assertRaises(RecoveryError):
                self.record()

    def test_missing_digest_and_ambiguous_artifact_refused(self):
        self.asset["digest"] = None
        with self.assertRaises(RecoveryError):
            primary_asset(self.release, "example-package", "python")
        self.release["assets"].append(deepcopy(self.asset))
        with self.assertRaises(RecoveryError):
            primary_asset(self.release, "example-package", "python")

    def test_npm_metadata_read_without_extracting_files(self):
        data = json.dumps({"name": "@juntai/example", "version": "1.2.3"}).encode()
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            info = tarfile.TarInfo("package/package.json")
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
        verify_package(buffer.getvalue(), "@juntai/example", "1.2.3", "npm")
        with self.assertRaises(RecoveryError):
            verify_package(buffer.getvalue(), "@juntai/other", "1.2.3", "npm")

    def test_workflow_reconciles_before_lock_through_existing_index_writer(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/jumbo-publish.yml").read_text()
        start = workflow.index("- name: Reconcile verified historical releases with JumboIndex")
        stop = workflow.index("- name:", start + 8)
        block = workflow[start:stop]
        self.assertIn("--push", block)
        self.assertIn("JUMBO_INDEX_TOKEN:", block)
        self.assertLess(start, workflow.index("- name: jumbo lock (resolve and generate the language lock)"))
        script = (root / "scripts/jumbo_reconcile_release.py").read_text()
        self.assertIn('with_name("jumbo_index_append.py")', script)
        self.assertNotIn("gh release create", script)


if __name__ == "__main__":
    unittest.main()
