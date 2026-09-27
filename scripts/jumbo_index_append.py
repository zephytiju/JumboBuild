#!/usr/bin/env python3
"""Append one record to a JumboIndex clone per docs/APPEND-PROTOCOL.md.

The JumboIndex append protocol (JumboIndex repository, docs/APPEND-PROTOCOL.md)
is the normative description; this script is the executor-side implementation
the `jumbo-publish` GitHub Actions workflow uses. In short:

1. Compute the record fully before touching the repository; serialize it as
   one line of canonical JSON (compact, `ensure_ascii=False`, the key order of
   schema/record.schema.json) terminated by a single ``\\n``.
2. Re-check the append preconditions against the current tip: no existing
   record for the same (package, version), no existing record with the same
   fingerprint, and a version strictly greater than the last record of the
   package file.
3. Append the line (nothing else changes) and commit with the message
   ``index: append <package>@<version> (<executor>)``.
4. Validate the index (``scripts/validate_index.py`` and the append-only
   history audit) on the new commit.
5. Push fast-forward only. A non-fast-forward rejection is the serialization
   point, not an error: fetch the new tip, re-check the preconditions on it
   (a concurrent append may already satisfy the intent — then report
   ``already-recorded`` and drop the local append), re-append on the fresh
   tip, and retry with bounded exponential backoff.

No history rewrites ever: no ``--force``, no ``--force-with-lease``, no
``commit --amend``, no edits to existing lines. The script only opens package
files in append mode.

Credentials: the push token is read from an environment variable (default
``JUMBO_INDEX_TOKEN``) and handed to git through the process environment
(``GIT_CONFIG_COUNT`` extraheader), never through argv, remotes, or files.
"""

from __future__ import annotations

import argparse
import base64
import ipaddress
import json
import os
import re
import subprocess
import sys
import time
import urllib.parse
from pathlib import Path
from typing import Any

# The canonical key order of schema/record.schema.json. Records are
# serialized exactly in this order, compact, one line.
RECORD_KEYS = (
    "package",
    "major",
    "version",
    "commit",
    "fingerprint",
    "canonicalExtract",
    "artifactUrl",
    "artifactSha256",
    "imageDigest",
    "buildId",
    "pipelineRun",
    "executor",
    "timestamp",
)

COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
IMAGE_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+[A-Za-z0-9.+-]*$")
TIMESTAMP_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$")

# Release-asset hosts the standard allows in artifactUrl (GitHub Releases
# assets on the package repository, §3.4) — the same allowlist the JumboIndex
# index-side validator enforces, so a record the executor accepts the index
# accepts too. Anything else (other hosts, localhost/loopback, private or
# reserved IP literals) is refused before it can reach the index.
ALLOWED_ARTIFACT_HOSTS = {"github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"}


class AppendError(RuntimeError):
    """A protocol refusal or an exhausted retry budget (never a rewrite)."""


# ---------------------------------------------------------------------------
# Record validation and canonical serialization
# ---------------------------------------------------------------------------

def validate_record(record: dict[str, Any]) -> None:
    """Schema-lite checks so a malformed record never reaches the index.

    The authoritative gate is JumboIndex's scripts/validate_index.py (run on
    the appended commit); these checks fail fast with clearer messages.
    """
    missing = [k for k in RECORD_KEYS if k not in record]
    extra = [k for k in record if k not in RECORD_KEYS]
    if missing:
        raise AppendError(f"record is missing keys: {', '.join(missing)}")
    if extra:
        raise AppendError(f"record has unknown keys: {', '.join(extra)}")

    package = record["package"]
    if not isinstance(package, str) or not package:
        raise AppendError("package must be a non-empty string")
    if not isinstance(record["major"], int) or isinstance(record["major"], bool):
        raise AppendError("major must be an integer")
    version = record["version"]
    if not isinstance(version, str) or not VERSION_RE.match(version):
        raise AppendError(f"version {version!r} must be X.Y.Z")
    if int(version.split(".", 1)[0]) != record["major"]:
        raise AppendError("major must equal the first segment of version")
    commit = record["commit"]
    if not isinstance(commit, str) or not COMMIT_RE.match(commit):
        raise AppendError("commit must be a full 40-hex-digit SHA")
    fingerprint = record["fingerprint"]
    if fingerprint is not None and (not isinstance(fingerprint, str) or not SHA256_RE.match(fingerprint)):
        raise AppendError("fingerprint must be a 64-hex digest or null")
    url = record["artifactUrl"]
    if url is not None:
        if not isinstance(url, str):
            raise AppendError("artifactUrl must be an https:// URL or null")
        try:
            parsed = urllib.parse.urlsplit(url)
        except ValueError as exc:
            raise AppendError(f"artifactUrl is not a parseable URL: {exc}") from exc
        if parsed.scheme != "https":
            raise AppendError("artifactUrl must be an https:// URL or null")
        host = parsed.hostname or ""
        if host.lower() not in ALLOWED_ARTIFACT_HOSTS:
            raise AppendError(f"artifactUrl host {host!r} is not in the release-asset allowlist")
        try:
            addr = ipaddress.ip_address(host)
            if not addr.is_global:
                raise AppendError("artifactUrl must not resolve to a non-global address")
        except ValueError:
            pass  # a hostname, not an IP literal
    sha = record["artifactSha256"]
    if sha is not None and (not isinstance(sha, str) or not SHA256_RE.match(sha)):
        raise AppendError("artifactSha256 must be a 64-hex digest or null")
    if sha is not None and url is None:
        raise AppendError("artifactSha256 set without artifactUrl")
    image_digest = record["imageDigest"]
    if image_digest is not None and (not isinstance(image_digest, str) or not IMAGE_DIGEST_RE.match(image_digest)):
        raise AppendError("imageDigest must be a sha256:<64-hex> image digest or null")
    if not isinstance(record["executor"], str) or not record["executor"]:
        raise AppendError("executor must be a non-empty string")
    timestamp = record["timestamp"]
    if not isinstance(timestamp, str) or not TIMESTAMP_RE.match(timestamp):
        raise AppendError("timestamp must be RFC 3339 UTC")


def canonical_line(record: dict[str, Any]) -> str:
    """The single JSONL line: canonical key order, compact, no ASCII escapes."""
    ordered = {key: record[key] for key in RECORD_KEYS}
    line = json.dumps(ordered, ensure_ascii=False, separators=(",", ":"))
    if "\n" in line:
        raise AppendError("record must serialize to a single line")
    return line + "\n"


def slug_for(package: str) -> str:
    return package.replace("@", "").replace("/", "-")


# ---------------------------------------------------------------------------
# Git helpers (no history rewrites anywhere)
# ---------------------------------------------------------------------------

def git(index_dir: Path, *args: str, token: str | None = None, host: str | None = None) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    if token is not None and host:
        # Push authentication through the process environment only: the
        # token never appears in argv, in a remote URL, or in a config file.
        basic = base64.b64encode(f"x-access-token:{token}".encode()).decode()
        env.update(
            GIT_CONFIG_COUNT="1",
            GIT_CONFIG_KEY_0=f"http.https://{host}/.extraheader",
            GIT_CONFIG_VALUE_0=f"AUTHORIZATION: basic {basic}",
        )
    return subprocess.run(
        ["git", "-C", str(index_dir), *args],
        capture_output=True,
        text=True,
        env=env,
    )


def require_git(index_dir: Path, *args: str, token: str | None = None, host: str | None = None) -> str:
    proc = git(index_dir, *args, token=token, host=host)
    if proc.returncode != 0:
        raise AppendError(f"git {' '.join(args)} failed: {proc.stderr.strip() or proc.stdout.strip()}")
    return proc.stdout.strip()


def remote_host(index_dir: Path, remote: str) -> str | None:
    url = require_git(index_dir, "remote", "get-url", remote)
    match = re.match(r"^https://([^/]+)/", url)
    return match.group(1) if match else None


def tree_is_clean(index_dir: Path) -> bool:
    return require_git(index_dir, "status", "--porcelain") == ""


# ---------------------------------------------------------------------------
# Precondition checks (APPEND-PROTOCOL step 2)
# ---------------------------------------------------------------------------

def load_lines(index_dir: Path, package: str) -> list[dict[str, Any]]:
    path = index_dir / "index" / f"{slug_for(package)}.jsonl"
    if not path.exists():
        return []
    records = []
    for lineno, line in enumerate(path.read_text().splitlines(), start=1):
        if not line.strip():
            raise AppendError(f"{path.name}:{lineno}: blank lines are not allowed")
        records.append(json.loads(line))
    return records


def iter_all_records(index_dir: Path):
    index_root = index_dir / "index"
    if not index_root.is_dir():
        return
    for path in sorted(index_root.glob("*.jsonl")):
        for lineno, line in enumerate(path.read_text().splitlines(), start=1):
            if line.strip():
                yield path, lineno, json.loads(line)


def version_key(version: str):
    match = re.match(r"(\d+)\.(\d+)\.(\d+)(.*)$", version)
    return (int(match.group(1)), int(match.group(2)), int(match.group(3)), match.group(4)) if match else None


def check_preconditions(index_dir: Path, record: dict[str, Any]) -> dict[str, Any] | None:
    """Return a conflicting record when the append must NOT happen.

    None means the preconditions hold: append. A conflicting record means a
    concurrent append already satisfied the intent — the caller reports
    already-recorded and drops the local append (idempotency).
    """
    existing = load_lines(index_dir, record["package"])
    for prior in existing:
        if prior["version"] == record["version"]:
            return {"reason": "version already recorded", "record": prior}
    fingerprint = record["fingerprint"]
    if fingerprint is not None:
        for path, lineno, prior in iter_all_records(index_dir):
            if prior.get("fingerprint") == fingerprint:
                return {
                    "reason": "fingerprint already recorded",
                    "record": prior,
                    "where": f"{path.name}:{lineno}",
                }
    if existing:
        last_key = version_key(existing[-1]["version"])
        new_key = version_key(record["version"])
        if last_key is not None and new_key is not None and new_key <= last_key:
            raise AppendError(
                f"version {record['version']} is not strictly greater than the last record "
                f"{existing[-1]['version']} of {record['package']}"
            )
    return None


# ---------------------------------------------------------------------------
# The append attempt
# ---------------------------------------------------------------------------

def validate_index(index_dir: Path) -> None:
    validator = index_dir / "scripts" / "validate_index.py"
    if not validator.exists():
        print("validate_index.py not found in the index clone; internal checks only", file=sys.stderr)
        return
    for args in (["--audit-history"], []):
        proc = subprocess.run(
            [sys.executable, str(validator), *args],
            capture_output=True,
            text=True,
            cwd=str(index_dir),
        )
        if proc.returncode != 0:
            output = (proc.stderr + proc.stdout).strip()
            raise AppendError(f"validate_index.py {' '.join(args) or '(schema)'} rejected the append:\n{output}")


def append_once(index_dir: Path, record: dict[str, Any], line: str) -> str:
    """Append the line, commit, and validate. Returns the commit SHA."""
    if not tree_is_clean(index_dir):
        raise AppendError("the index clone has uncommitted changes; refusing to touch it")

    target = index_dir / "index" / f"{slug_for(record['package'])}.jsonl"
    existing_lines = target.read_text().splitlines() if target.exists() else []
    if existing_lines:
        first = json.loads(existing_lines[0])
        if first.get("package") != record["package"]:
            raise AppendError(
                f"{target.name} belongs to package {first.get('package')!r}, not {record['package']!r}"
            )

    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open("a", encoding="utf-8") as handle:
        handle.write(line)

    message = f"index: append {record['package']}@{record['version']} ({record['executor']})"
    require_git(index_dir, "add", str(target.relative_to(index_dir)))
    require_git(
        index_dir,
        "-c",
        f"user.name={os.environ.get('JUMBO_GIT_USER_NAME', 'jumbo-index-append')}",
        "-c",
        f"user.email={os.environ.get('JUMBO_GIT_USER_EMAIL', 'jumbo-publish@users.noreply.github.com')}",
        "commit", "-m", message,
    )

    try:
        validate_index(index_dir)
    except AppendError:
        # Validation rejected the append: discard our own commit. It was
        # never pushed, so nothing published is rewritten; the index clone
        # returns to the exact state it had before the attempt.
        require_git(index_dir, "reset", "--hard", "HEAD~1")
        raise
    return require_git(index_dir, "rev-parse", "HEAD")


def append_with_retry(
    index_dir: Path,
    record: dict[str, Any],
    *,
    push: bool,
    remote: str,
    branch: str,
    token: str | None,
    max_attempts: int,
    backoff_base: float,
) -> dict[str, Any]:
    """One append attempt, then fetch-and-retry on non-fast-forward pushes."""
    line = canonical_line(record)
    host = remote_host(index_dir, remote) if push else None
    attempts = 0
    last_error = ""

    while True:
        attempts += 1
        conflict = check_preconditions(index_dir, record)
        if conflict is not None:
            return {
                "status": "already-recorded",
                "package": record["package"],
                "version": record["version"],
                "conflictReason": conflict["reason"],
                "conflict": conflict["record"],
                "attempts": attempts,
                "pushed": False,
            }

        commit = append_once(index_dir, record, line)

        if not push:
            return {
                "status": "appended",
                "package": record["package"],
                "version": record["version"],
                "file": f"index/{slug_for(record['package'])}.jsonl",
                "commit": commit,
                "attempts": attempts,
                "pushed": False,
            }

        # A plain push (no force flags of any kind) is fast-forward-only by
        # git semantics: the remote rejects a non-fast-forward update unless
        # the client forces, and this script never passes a force flag.
        proc = git(index_dir, "push", remote, f"HEAD:refs/heads/{branch}", token=token, host=host)
        if proc.returncode == 0:
            return {
                "status": "appended",
                "package": record["package"],
                "version": record["version"],
                "file": f"index/{slug_for(record['package'])}.jsonl",
                "commit": commit,
                "attempts": attempts,
                "pushed": True,
            }

        last_error = (proc.stderr + proc.stdout).strip()
        # Surface the push error immediately: the retry below must never
        # mask why the push failed (an auth/permission failure and a race
        # both land here, and only this message tells them apart).
        print(f"jumbo_index_append: push attempt {attempts} failed:\n{last_error}", file=sys.stderr)
        if attempts >= max_attempts:
            raise AppendError(
                f"push still failing after {attempts} attempts (bounded backoff exhausted); "
                f"the local append commit {commit} was NOT rewritten — fail loudly, never "
                f"force. Last push error:\n{last_error}"
            )

        # Non-fast-forward (or a transient failure): fetch the new tip,
        # re-check the preconditions on it, and re-append. Discarding our
        # own unpushed commit by resetting to the fetched tip is the
        # protocol's rebase-and-retry with append-only semantics — nothing
        # published is ever rewritten. The fetch carries the same token as
        # the push: over https the index repository is private, and an
        # anonymous fetch cannot even read it.
        require_git(index_dir, "fetch", remote, branch, token=token, host=host)
        fetched = require_git(index_dir, "rev-parse", "FETCH_HEAD")
        require_git(index_dir, "reset", "--hard", fetched)
        sleep_seconds = min(backoff_base * (2 ** (attempts - 1)), 60.0)
        if sleep_seconds > 0:
            time.sleep(sleep_seconds)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--index-dir", required=True, type=Path, help="local clone of JumboIndex")
    record_group = parser.add_mutually_exclusive_group(required=True)
    record_group.add_argument("--record-file", type=Path, help="path to a JSON record object")
    record_group.add_argument("--record", help="a JSON record object as a string")
    parser.add_argument("--push", action="store_true", help="push the append (fast-forward only)")
    parser.add_argument("--token-env", default="JUMBO_INDEX_TOKEN", help="env var carrying the push token")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--branch", default="main")
    parser.add_argument("--max-attempts", type=int, default=5, help="push retry budget (default 5)")
    parser.add_argument("--backoff-base", type=float, default=1.0, help="exponential backoff base seconds (default 1)")
    args = parser.parse_args(argv)

    if not args.index_dir.is_dir():
        print(f"index directory not found: {args.index_dir}", file=sys.stderr)
        return 2

    raw = Path(args.record_file).read_text() if args.record_file else args.record
    try:
        record = json.loads(raw)
    except json.JSONDecodeError as error:
        print(f"record is not valid JSON: {error}", file=sys.stderr)
        return 2
    if not isinstance(record, dict):
        print("record must be a JSON object", file=sys.stderr)
        return 2

    token = None
    if args.push:
        host = remote_host(args.index_dir, args.remote)
        if host:  # https remotes need a token; file:// test remotes do not
            token = os.environ.get(args.token_env)
            if not token:
                print(
                    f"pushing over https requires the token env var {args.token_env} "
                    "(a GitHub App/user token with push access to the index repository)",
                    file=sys.stderr,
                )
                return 2

    try:
        validate_record(record)
        result = append_with_retry(
            args.index_dir,
            record,
            push=args.push,
            remote=args.remote,
            branch=args.branch,
            token=token,
            max_attempts=max(1, args.max_attempts),
            backoff_base=max(0.0, args.backoff_base),
        )
    except AppendError as error:
        print(f"jumbo_index_append: {error}", file=sys.stderr)
        return 1

    print(json.dumps(result, ensure_ascii=False, indent=2))
    if result["status"] == "already-recorded":
        print(
            "note: a concurrent append already satisfied this intent "
            f"({result['conflictReason']}); reusing the recorded record",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
