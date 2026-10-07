#!/usr/bin/env python3
"""Reconcile a verified pre-Jumbo release through the normal publication job.

No release, tag or existing index row is modified. The only writer is the
existing validator-gated, fast-forward index append client. Promotion itself
continues to derive solely from the index. Default mode is read-only.
"""

from __future__ import annotations

import argparse
import base64
import email.parser
import hashlib
import io
import json
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from datetime import UTC, datetime
from pathlib import Path
from urllib.parse import quote

from jumbo_index_append import load_lines, validate_record

VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
MAX_ARTIFACT_BYTES = 256 * 1024 * 1024
MAX_METADATA_BYTES = 1024 * 1024


class RecoveryError(ValueError):
    pass


def version(value):
    match = VERSION.fullmatch(value)
    if match is None:
        raise RecoveryError("exact stable semantic version required")
    return tuple(map(int, match.groups()))


def normalized(name):
    return re.sub(r"[-_.]+", "-", name).lower()


def manifest_identity(path):
    return parse_manifest(path, path.read_text())


def unique_json(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise RecoveryError("ambiguous JSON metadata")
        result[key] = value
    return result


def parse_manifest(path, content):
    if path.name == "pyproject.toml":
        data = tomllib.loads(content)["project"]
        ecosystem = "python"
    elif path.name == "package.json":
        data = json.loads(content, object_pairs_hook=unique_json)
        ecosystem = "npm"
    else:
        raise RecoveryError("unsupported manifest")
    name, own_version = data["name"], data["version"]
    if not isinstance(name, str) or not name or not re.fullmatch(r"[A-Za-z0-9@/_.-]+", name):
        raise RecoveryError("invalid package name")
    return name, version(own_version)[0], ecosystem


def select_release(releases, records, package, major, *, inspect_release):
    """Rank only releases verified for this identity; never downgrade history.

    The inspector must return true for a verified current-package release,
    false only for a proven other identity, and raise for unknown/contradictory
    identity or damaged current-package history. Inspect every eligible release
    before ranking so a higher valid release cannot hide a damaged lower one.
    """
    for row in records:
        if row["package"] != package:
            raise RecoveryError("index package identity mismatch")
    floor = max((version(row["version"]) for row in records), default=(-1, -1, -1))
    candidates = []
    for release in releases:
        tag = release.get("tag_name", "")
        if release.get("draft") or release.get("prerelease") or not tag.startswith("v"):
            continue
        if not VERSION.fullmatch(tag[1:]):
            continue
        key = version(tag[1:])
        if key[0] == major and key > floor:
            if inspect_release(release):
                candidates.append((key, release))
    if not candidates:
        return None
    candidates.sort(key=lambda item: item[0])
    key, release = candidates[-1]
    if sum(item[0] == key for item in candidates) != 1:
        raise RecoveryError("ambiguous historical release")
    if release.get("immutable") is not True:
        raise RecoveryError("historical release must be immutable before indexing")
    # An interrupted Jumbo publication requires its original build record,
    # not a bootstrap record that discards the known build provenance.
    if "jumbo release of **" in (release.get("body") or ""):
        raise RecoveryError("interrupted Jumbo publication requires its original promotion record")
    return release


def matching_assets(release, package, ecosystem):
    tag_version = release["tag_name"][1:]
    if ecosystem == "python":
        prefix = normalized(package).replace("-", "_") + "-" + tag_version + "-"
        assets = [a for a in release["assets"] if a["name"].startswith(prefix) and a["name"].endswith(".whl")]
    else:
        stem = package.lstrip("@").replace("/", "-")
        assets = [a for a in release["assets"] if a["name"] == f"{stem}-{tag_version}.tgz"]
    return assets


def primary_asset(release, package, ecosystem):
    assets = matching_assets(release, package, ecosystem)
    if len(assets) != 1:
        raise RecoveryError("exactly one primary package artifact required")
    asset = assets[0]
    if not isinstance(asset.get("size"), int) or not 0 < asset["size"] <= MAX_ARTIFACT_BYTES:
        raise RecoveryError("historical artifact exceeds the byte bound")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", asset.get("digest") or ""):
        raise RecoveryError("GitHub asset SHA-256 is required")
    if not isinstance(asset.get("id"), int) or asset["id"] <= 0:
        raise RecoveryError("invalid release asset identity")
    return asset


def verify_package(data, package, release_version, ecosystem):
    if ecosystem == "python":
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            entries = [info for info in archive.infolist() if info.filename.endswith(".dist-info/METADATA")]
            if len(entries) != 1 or entries[0].file_size > MAX_METADATA_BYTES:
                raise RecoveryError("bounded unique wheel metadata required")
            fields = email.parser.BytesParser().parsebytes(archive.read(entries[0]))
            names, versions = fields.get_all("Name", []), fields.get_all("Version", [])
            if len(names) != 1 or len(versions) != 1:
                raise RecoveryError("unique package name/version required")
            valid_name = normalized(names[0]) == normalized(package)
            actual_version = versions[0]
    else:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            entries = [item for item in archive if item.name == "package/package.json"]
            if len(entries) != 1 or not entries[0].isfile() or entries[0].size > MAX_METADATA_BYTES:
                raise RecoveryError("bounded unique npm metadata required")
            with archive.extractfile(entries[0]) as stream:
                fields = json.load(stream, object_pairs_hook=unique_json)
            valid_name = fields.get("name") == package
            actual_version = fields.get("version")
    if not valid_name or actual_version != release_version:
        raise RecoveryError("historical package name/version differs from the release")


def make_record(release, asset, data, *, repository, package, major, ecosystem, commit):
    if not COMMIT.fullmatch(commit):
        raise RecoveryError("tag must resolve to an exact source commit")
    release_version = release["tag_name"][1:]
    if version(release_version)[0] != major:
        raise RecoveryError("historical release major differs")
    expected_url = f"https://github.com/{repository}/releases/download/{release['tag_name']}/{asset['name']}"
    digest = hashlib.sha256(data).hexdigest()
    if asset.get("browser_download_url") != expected_url:
        raise RecoveryError("historical artifact is not on the caller repository")
    if len(data) != asset["size"] or "sha256:" + digest != asset["digest"]:
        raise RecoveryError("historical artifact bytes differ from GitHub provenance")
    verify_package(data, package, release_version, ecosystem)
    record = {
        "package": package, "major": major, "version": release_version, "commit": commit,
        "fingerprint": None, "canonicalExtract": None,
        "artifactUrl": expected_url, "artifactSha256": digest,
        "imageDigest": None, "buildId": None, "pipelineRun": None,
        "executor": "bootstrap", "timestamp": datetime.now(UTC).isoformat(),
    }
    validate_record(record)
    return record


def gh_json(endpoint, *options):
    return json.loads(subprocess.check_output(["gh", "api", endpoint, *options], text=True))


def tagged_commit(repository, tag):
    target = gh_json(f"repos/{repository}/git/ref/tags/{tag}")["object"]
    for _ in range(5):
        if target["type"] == "commit" and COMMIT.fullmatch(target["sha"]):
            return target["sha"]
        if target["type"] != "tag" or not COMMIT.fullmatch(target["sha"]):
            break
        target = gh_json(f"repos/{repository}/git/tags/{target['sha']}")["object"]
    raise RecoveryError("historical tag does not resolve to a bounded commit")


def tagged_manifest(repository, commit, manifest):
    """Read the same manifest path at the peeled tag commit, never a branch."""
    if manifest.is_absolute():
        try:
            manifest = manifest.relative_to(Path.cwd())
        except ValueError as error:
            raise RecoveryError("source manifest is outside the caller repository") from error
    if not COMMIT.fullmatch(commit) or ".." in manifest.parts:
        raise RecoveryError("tag-bound manifest requires an exact commit and repository-relative path")
    item = gh_json(f"repos/{repository}/contents/{quote(manifest.as_posix(), safe='/')}?ref={commit}")
    if (not isinstance(item, dict) or item.get("type") != "file"
            or item.get("path") != manifest.as_posix() or item.get("encoding") != "base64"
            or not isinstance(item.get("size"), int) or not 0 < item["size"] <= MAX_METADATA_BYTES):
        raise RecoveryError("bounded tag-bound source manifest required")
    data = base64.b64decode("".join(item["content"].split()), validate=True)
    if len(data) != item["size"]:
        raise RecoveryError("tag-bound manifest bytes differ from source metadata")
    return parse_manifest(manifest, data.decode("utf-8"))


def download_asset(repository, asset):
    with tempfile.TemporaryDirectory(prefix="jumbo-historical-release-") as directory:
        path = Path(directory) / "artifact"
        with path.open("wb") as output:
            subprocess.run([
                "gh", "api", f"repos/{repository}/releases/assets/{asset['id']}",
                "-H", "Accept: application/octet-stream",
            ], stdout=output, check=True)
        if path.stat().st_size > MAX_ARTIFACT_BYTES:
            raise RecoveryError("download exceeds the historical artifact byte bound")
        return path.read_bytes()


def inspect_history(release, *, repository, manifest, package, major, ecosystem):
    """Return a verified adoption record, or None for a proven other package."""
    commit = tagged_commit(repository, release["tag_name"])
    source_package, source_major, source_ecosystem = tagged_manifest(repository, commit, manifest)
    same_identity = (normalized(source_package) == normalized(package) if ecosystem == "python"
                     else source_package == package)
    if source_ecosystem != ecosystem or source_major != major:
        raise RecoveryError("tag-bound source ecosystem/major differs from historical release")
    # Preserve original promotion provenance even when source identity changed.
    if "jumbo release of **" in (release.get("body") or ""):
        raise RecoveryError("interrupted Jumbo publication requires its original promotion record")
    if same_identity:
        if release.get("immutable") is not True:
            raise RecoveryError("historical release must be immutable before indexing")
    elif matching_assets(release, package, ecosystem):
        raise RecoveryError("historical artifact contradicts tag-bound package identity")
    elif not any(a["name"].endswith((".whl", ".tgz")) for a in release["assets"]):
        # Source proves the rename even for an assetless mutable legacy release.
        return None
    # If package artifacts exist, source identity must agree with the unique
    # expected primary artifact AND its actual bytes/name/version/digest.
    asset = primary_asset(release, source_package, ecosystem)
    if any(a != asset and a["name"].endswith((".whl", ".tgz")) for a in release["assets"]):
        raise RecoveryError("ambiguous historical package artifacts")
    record = make_record(
        release, asset, download_asset(repository, asset), repository=repository,
        package=source_package, major=major, ecosystem=ecosystem, commit=commit,
    )
    if not same_identity:
        return None
    record["package"] = package
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--index-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--push", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repository):
        raise RecoveryError("invalid caller repository")
    manifest = args.manifest or (Path("pyproject.toml") if Path("pyproject.toml").exists() else Path("package.json"))
    package, major, ecosystem = manifest_identity(manifest)
    pages = gh_json(f"repos/{args.repository}/releases?per_page=100", "--paginate", "--slurp")
    releases = [release for page in pages for release in page]
    verified = {}

    def inspect(release):
        record = inspect_history(
            release, repository=args.repository, manifest=manifest,
            package=package, major=major, ecosystem=ecosystem,
        )
        if record is None:
            return False
        verified[id(release)] = record
        return True

    release = select_release(
        releases, load_lines(args.index_dir, package), package, major, inspect_release=inspect,
    )
    if release is None:
        print(json.dumps({"status": "already-aligned", "package": package}))
        return 0
    record = verified[id(release)]
    args.output.write_text(json.dumps(record, ensure_ascii=False) + "\n")
    if args.push:
        result = json.loads(subprocess.check_output([
            sys.executable, str(Path(__file__).with_name("jumbo_index_append.py")),
            "--index-dir", str(args.index_dir), "--record-file", str(args.output),
            "--push", "--token-env", "JUMBO_INDEX_TOKEN",
        ], text=True))
        # An equal version won concurrently only if it names these same bytes.
        current = [r for r in load_lines(args.index_dir, package) if r["version"] == record["version"]]
        if len(current) != 1 or any(current[0][k] != record[k] for k in ("commit", "artifactUrl", "artifactSha256")):
            raise RecoveryError("concurrent historical record does not match verified release")
        print(json.dumps({"status": result["status"], "package": package, "version": record["version"]}))
    else:
        print(json.dumps({"status": "verified-read-only", "package": package, "version": record["version"]}))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RecoveryError, KeyError, ValueError, subprocess.CalledProcessError) as error:
        print(f"jumbo historical release recovery: {error}", file=sys.stderr)
        raise SystemExit(1)
