"""Offline historical release recovery; no release/index/network mutation."""

import hashlib
import base64
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest
import zipfile
from copy import deepcopy
from unittest.mock import patch
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from jumbo_reconcile_release import (  # noqa: E402
    RecoveryError, inspect_history, main, make_record, primary_asset, select_release, tagged_commit,
    tagged_manifest, verify_package,
)


def wheel(name="example-package", version="0.7.4"):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("example_package-0.7.4.dist-info/METADATA", f"Name: {name}\nVersion: {version}\n")
    return buffer.getvalue()


def npm(name, version):
    data = json.dumps({"name": name, "version": version}).encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        info = tarfile.TarInfo("package/package.json")
        info.size = len(data)
        archive.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


class IdentitySelectionTests(unittest.TestCase):
    def setUp(self):
        self.package = "@juntai/juntai-console"
        self.old_package = "@zephytiju/juntai-console"
        self.records = [{"package": self.package, "version": "2.0.0"}]
        self.legacy = {"tag_name": "v2.7.1", "immutable": False, "draft": False,
                       "prerelease": False, "assets": [], "body": "Legacy native release"}
        self.source = patch("jumbo_reconcile_release.tagged_manifest", return_value=(self.old_package, 2, "npm")).start()
        self.commit = patch("jumbo_reconcile_release.tagged_commit", return_value="a" * 40).start()
        self.download = patch("jumbo_reconcile_release.download_asset").start()
        self.addCleanup(patch.stopall)

    def inspect(self, release):
        return inspect_history(release, repository="org/repo", manifest=Path("package.json"),
                               package=self.package, major=2, ecosystem="npm")

    def select(self, releases):
        return select_release(releases, self.records, self.package, 2, inspect_release=self.inspect)

    def artifact(self, package, release=None):
        release = deepcopy(release or self.legacy)
        v = release["tag_name"][1:]
        data = npm(package, v)
        name = f"{package.lstrip('@').replace('/', '-')}-{v}.tgz"
        release["assets"] = [{"id": 12, "name": name, "size": len(data),
                              "digest": "sha256:" + hashlib.sha256(data).hexdigest(),
                              "browser_download_url": f"https://github.com/org/repo/releases/download/v{v}/{name}"}]
        self.download.return_value = data
        return release

    def test_console_assetless_mutable_rename_is_ignored(self):
        self.assertIsNone(self.select([self.legacy]))
        self.commit.assert_called_once_with("org/repo", "v2.7.1")
        self.source.assert_called_once_with("org/repo", "a" * 40, Path("package.json"))
        self.download.assert_not_called()

    def test_other_identity_with_matching_source_and_artifact_is_ignored(self):
        self.assertIsNone(self.select([self.artifact(self.old_package)]))
        self.download.assert_called_once()

    def test_current_identity_recovery_ranks_after_ignoring_higher_legacy(self):
        current = self.artifact(self.package, dict(self.legacy, tag_name="v2.1.0", immutable=True))
        self.source.side_effect = [(self.old_package, 2, "npm"), (self.package, 2, "npm")]
        self.assertEqual(self.select([self.legacy, current]), current)

    def test_same_identity_recovery_preserves_exact_source_and_bytes(self):
        self.source.return_value = (self.package, 2, "npm")
        current = self.artifact(self.package, dict(self.legacy, immutable=True))
        record = self.inspect(current)
        self.assertEqual(record["commit"], "a" * 40)
        self.assertEqual(record["package"], self.package)
        self.assertEqual(record["artifactSha256"], hashlib.sha256(self.download.return_value).hexdigest())
        self.assertEqual(record["executor"], "bootstrap")
        self.assertIsNone(record["buildId"])

    def test_unknown_source_or_unresolved_tag_is_not_ignored(self):
        for target in (self.source, self.commit):
            target.side_effect = RecoveryError("unknown source identity")
            with self.assertRaisesRegex(RecoveryError, "unknown source"):
                self.select([self.legacy])
            target.side_effect = None

    def test_contradictory_current_artifact_on_other_source_is_refused(self):
        with self.assertRaisesRegex(RecoveryError, "contradicts"):
            self.select([self.artifact(self.package)])

    def test_ambiguous_other_package_artifacts_are_refused(self):
        release = self.artifact(self.old_package)
        release["assets"].append(deepcopy(release["assets"][0]))
        with self.assertRaisesRegex(RecoveryError, "exactly one"):
            self.select([release])

    def test_secondary_package_artifact_makes_release_identity_ambiguous(self):
        for source in (self.old_package, self.package):
            self.source.return_value = (source, 2, "npm")
            release = self.artifact(source, dict(self.legacy, immutable=True))
            release["assets"].append(dict(release["assets"][0], name="unidentified-2.7.1.tgz"))
            with self.subTest(source=source), self.assertRaisesRegex(RecoveryError, "ambiguous historical package"):
                self.select([release])

    def test_artifact_bytes_must_match_other_source_before_ignore(self):
        release = self.artifact(self.old_package)
        self.download.return_value = npm(self.package, "2.7.1")
        with self.assertRaises(RecoveryError):
            self.select([release])
        data = self.download.return_value
        release["assets"][0].update(size=len(data), digest="sha256:" + hashlib.sha256(data).hexdigest())
        with self.assertRaisesRegex(RecoveryError, "name/version differs"):
            self.select([release])

    def test_current_mutable_missing_corrupt_or_ambiguous_assets_refused(self):
        self.source.return_value = (self.package, 2, "npm")
        with self.assertRaisesRegex(RecoveryError, "immutable"):
            self.select([self.legacy])
        with self.assertRaisesRegex(RecoveryError, "exactly one"):
            self.select([dict(self.legacy, immutable=True)])
        release = self.artifact(self.package, dict(self.legacy, immutable=True))
        self.download.return_value += b"corrupt"
        with self.assertRaisesRegex(RecoveryError, "bytes differ"):
            self.select([release])
        release["assets"].append(deepcopy(release["assets"][0]))
        with self.assertRaisesRegex(RecoveryError, "exactly one"):
            self.select([release])

    def test_current_malformed_lower_release_cannot_hide_behind_higher(self):
        self.source.return_value = (self.package, 2, "npm")
        valid = self.artifact(self.package, dict(self.legacy, immutable=True))
        broken = dict(self.legacy, tag_name="v2.1.0", immutable=True)
        with self.assertRaisesRegex(RecoveryError, "exactly one"):
            self.select([valid, broken])

    def test_interrupted_jumbo_history_preserves_provenance_for_any_identity(self):
        for name in (self.old_package, self.package):
            self.source.return_value = (name, 2, "npm")
            with self.subTest(name=name), self.assertRaisesRegex(RecoveryError, "original promotion record"):
                self.select([dict(self.legacy, body=f"jumbo release of **{name}**")])

    def test_duplicate_current_release_version_is_ambiguous(self):
        self.source.return_value = (self.package, 2, "npm")
        current = self.artifact(self.package, dict(self.legacy, immutable=True))
        with self.assertRaisesRegex(RecoveryError, "ambiguous historical"):
            self.select([current, deepcopy(current)])

    def test_source_major_or_ecosystem_conflicts_refused(self):
        for identity in [(self.old_package, 1, "npm"), (self.old_package, 2, "python")]:
            self.source.return_value = identity
            with self.subTest(identity=identity), self.assertRaisesRegex(RecoveryError, "ecosystem/major"):
                self.select([self.legacy])

    def test_main_console_history_is_aligned_without_index_or_output_write(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "package.json"
            manifest.write_text(json.dumps({"name": self.package, "version": "2.0.0"}))
            output = Path(directory) / "record.json"
            with patch("sys.argv", ["reconcile", "--manifest", str(manifest), "--repository", "org/repo",
                                    "--index-dir", directory, "--output", str(output), "--push"]), \
                    patch("jumbo_reconcile_release.gh_json", return_value=[[self.legacy]]), \
                    patch("jumbo_reconcile_release.load_lines", return_value=self.records), \
                    patch("jumbo_reconcile_release.subprocess.check_output") as append:
                self.assertEqual(main(), 0)
                self.assertFalse(output.exists())
                append.assert_not_called()


class TagBoundManifestTests(unittest.TestCase):
    def item(self, content, path="package.json"):
        data = content.encode()
        return {"type": "file", "path": path, "size": len(data), "encoding": "base64",
                "content": base64.b64encode(data).decode()}

    def test_exact_peeled_commit_and_same_manifest_path(self):
        with patch("jumbo_reconcile_release.gh_json", return_value=self.item(
                '{"name":"@zephytiju/juntai-console","version":"2.7.1"}')) as api:
            self.assertEqual(tagged_manifest("org/repo", "a" * 40, Path("package.json")),
                             ("@zephytiju/juntai-console", 2, "npm"))
            api.assert_called_once_with("repos/org/repo/contents/package.json?ref=" + "a" * 40)

    def test_absolute_manifest_within_caller_keeps_same_relative_source_path(self):
        with patch("jumbo_reconcile_release.gh_json", return_value=self.item(
                '{"name":"example","version":"2.0.0"}')) as api:
            tagged_manifest("org/repo", "a" * 40, Path.cwd() / "package.json")
            api.assert_called_once_with("repos/org/repo/contents/package.json?ref=" + "a" * 40)

    def test_unknown_ambiguous_or_missing_manifest_identity_fails(self):
        for content in ['{}', '{"name":"one","name":"two","version":"2.0.0"}',
                        '{"name":"one","version":"invalid"}', 'not-json']:
            with self.subTest(content=content), patch("jumbo_reconcile_release.gh_json", return_value=self.item(content)), \
                    self.assertRaises((RecoveryError, KeyError, ValueError)):
                tagged_manifest("org/repo", "a" * 40, Path("package.json"))

    def test_manifest_source_bounds_path_and_commit_are_enforced(self):
        item = self.item('{"name":"one","version":"2.0.0"}')
        for changes in [{"type": "symlink"}, {"path": "other/package.json"}, {"size": 0},
                        {"size": 1024 * 1024 + 1}, {"encoding": "none"}, {"size": 1}]:
            with self.subTest(changes=changes), patch("jumbo_reconcile_release.gh_json", return_value=dict(item, **changes)), \
                    self.assertRaises(RecoveryError):
                tagged_manifest("org/repo", "a" * 40, Path("package.json"))
        for commit, path in [("main", "package.json"), ("a" * 40, "../package.json"), ("a" * 40, "/package.json")]:
            with self.subTest(commit=commit, path=path), self.assertRaises(RecoveryError):
                tagged_manifest("org/repo", commit, Path(path))

    def test_python_normalized_identity_still_recovers_exact_artifact(self):
        content = '[project]\nname="Example_Package"\nversion="0.7.4"\n'
        with patch("jumbo_reconcile_release.gh_json", return_value=self.item(content, "pyproject.toml")):
            self.assertEqual(tagged_manifest("org/repo", "a" * 40, Path("pyproject.toml")),
                             ("Example_Package", 0, "python"))

    def test_annotated_tag_is_peeled_before_manifest_access(self):
        with patch("jumbo_reconcile_release.gh_json", side_effect=[
                {"object": {"type": "tag", "sha": "b" * 40}},
                {"object": {"type": "commit", "sha": "a" * 40}}]) as api:
            self.assertEqual(tagged_commit("org/repo", "v2.7.1"), "a" * 40)
            self.assertEqual(api.call_args_list[1].args, ("repos/org/repo/git/tags/" + "b" * 40,))

    def test_unresolved_tag_chain_refuses_instead_of_using_target_branch(self):
        with patch("jumbo_reconcile_release.gh_json", return_value={"object": {"type": "tag", "sha": "b" * 40}}), \
                self.assertRaisesRegex(RecoveryError, "bounded commit"):
            tagged_commit("org/repo", "v2.7.1")


class PublicationCollisionTests(unittest.TestCase):
    def test_existing_release_refusal_aborts_without_success_or_overwrite(self):
        # Execute the unchanged production shell step with an offline gh that
        # refuses the existing release. Refusal must stop before output signals;
        # the trace must contain only create, never delete/edit/upload/tag writes.
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/jumbo-publish.yml").read_text()
        start = workflow.index("      - name: Publish the GitHub Release on the caller repository")
        stop = workflow.index("      # 11.", start)
        block = workflow[start:stop].split("        run: |\n", 1)[1]
        block = textwrap.dedent(block).replace("${{ steps.checksums.outputs.asset-name }}", "example.tgz")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "dist").mkdir()
            (path / "dist/example.tgz").write_bytes(b"fixture")
            (path / "promote.json").write_text('{"publish":{"fingerprint":"fixture"}}')
            (path / "gh").write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$TRACE"\necho "release already exists" >&2\nexit 1\n')
            (path / "gh").chmod(0o755)
            output = path / "outputs"
            trace = path / "trace"
            env = dict(os.environ, PATH=str(path) + os.pathsep + os.environ["PATH"], GH_TOKEN="offline-fixture",
                       JUMBO_VERSION="2.1.0", JUMBO_PACKAGE="@juntai/juntai-console", JUMBO_BUMP="minor",
                       JUMBO_COMMIT="a" * 40, JUMBO_OUT=directory, JUMBO_EXECUTOR="fixture",
                       GITHUB_SERVER_URL="https://github.com", GITHUB_REPOSITORY="org/repo",
                       GITHUB_RUN_ID="1", GITHUB_OUTPUT=str(output), TRACE=str(trace))
            result = subprocess.run(["bash", "-c", block], cwd=path, env=env, text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("release already exists", result.stderr)
            self.assertFalse(output.exists())
            commands = trace.read_text().splitlines()
            self.assertEqual(len(commands), 1)
            self.assertTrue(commands[0].startswith("release create v2.1.0 "))
            self.assertNotIn("--clobber", commands[0])


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
        selected = select_release([self.release], self.records, "example-package", 0, inspect_release=lambda _: True)
        self.assertEqual(primary_asset(selected, "example-package", "python"), self.asset)
        record = self.record()
        self.assertEqual(record["version"], "0.7.4")
        self.assertEqual(record["executor"], "bootstrap")
        self.assertEqual(record["artifactSha256"], hashlib.sha256(self.data).hexdigest())
        for field in ["fingerprint", "canonicalExtract", "buildId", "imageDigest", "pipelineRun"]:
            self.assertIsNone(record[field])

    def test_aligned_history_is_idempotent(self):
        self.records.append({"package": "example-package", "version": "0.7.4"})
        self.assertIsNone(select_release([self.release], self.records, "example-package", 0, inspect_release=lambda _: True))

    def test_latest_stable_same_major_only(self):
        other = [dict(self.release, tag_name=tag) for tag in ["v0.7.0", "v0.8.0-rc.1", "v1.0.0"]]
        self.assertEqual(select_release(other + [self.release], self.records, "example-package", 0, inspect_release=lambda _: True), self.release)

    def test_does_not_append_older_major_after_newer_major_history(self):
        self.records.append({"package": "example-package", "version": "1.0.0"})
        self.assertIsNone(select_release([self.release], self.records, "example-package", 0, inspect_release=lambda _: True))

    def test_mutable_or_interrupted_jumbo_release_refused(self):
        for changes in [{"immutable": False}, {"body": "jumbo release of **example-package**"}]:
            with self.subTest(changes=changes), self.assertRaises(RecoveryError):
                select_release([dict(self.release, **changes)], self.records, "example-package", 0, inspect_release=lambda _: True)

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
