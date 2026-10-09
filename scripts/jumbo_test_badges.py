#!/usr/bin/env python3
"""Trusted, credential-free rendering and fast-forward publication of test status.

Only GitHub API job/step metadata is consumed. Never download artifacts, check
out consumer code, execute test output, or accept paths/URLs from a test runner.
See docs/test-badges.md. GH_TOKEN is read by gh; git push auth is supplied by CI.
"""
from __future__ import annotations

import argparse
from datetime import datetime
from html import escape
import json
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import quote, urlencode

TEST_STEPS = {"Jumbo tests (python)", "Jumbo tests (npm)"}
STATUSES = {
    "passed": ("passing", "#4c1"),
    "failed": ("failing", "#e05d44"),
    "error": ("infrastructure error", "#e05d44"),
    "not-run": ("not run", "#9f9f9f"),
    "unknown": ("unverified", "#9f9f9f"),
}


def command(*args: str, cwd: Path | None = None) -> str:
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def api(path: str):
    return json.loads(command("gh", "api", "--hostname", "github.com", path))


def pages(path: str, key: str) -> list:
    result = []
    for page in range(1, 101):
        data = api(f"{path}{'&' if '?' in path else '?'}per_page=100&page={page}")
        result.extend(data[key])
        if len(data[key]) < 100:
            return result
    raise ValueError("GitHub pagination exceeded 10,000 entries; refusing partial results")


def component(value: str) -> str:
    if not isinstance(value, str) or not value or value in {".", ".."}:
        raise ValueError("invalid path component")
    return quote(value, safe="")


def coordinates(source: dict) -> tuple[str, str]:
    repo = source["repository"]
    if not re.fullmatch(r"[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+", repo):
        raise ValueError("source repository must be owner/name")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+\.yml", source["workflow"]):
        raise ValueError("workflow must be a YAML filename")
    return repo, source["workflow"]


def iso(value: str) -> str:
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("completion time must include a timezone")
    return value


def result_from_run(source: dict, branch: str, run: dict, jobs: list, producer_sha: str) -> dict:
    repo, workflow = coordinates(source)
    if (run["event"] != "push" or run["head_branch"] != branch
            or run["status"] != "completed"
            or run["head_repository"]["full_name"] != repo
            or run["path"] != f".github/workflows/{workflow}"):
        raise ValueError("run is not an authorized default-branch verification")
    if not re.fullmatch(r"[0-9a-f]{40}", run["head_sha"]):
        raise ValueError("invalid source commit")
    run_id, attempt = run["id"], run["run_attempt"]
    if type(run_id) is not int or type(attempt) is not int or min(run_id, attempt) < 1:
        raise ValueError("invalid run identity")
    # Names AND the approved reusable workflow SHA bind the measurements to
    # Jumbo's producer contract; legacy/unapproved workflows cannot turn green.
    approved = any(
        ref.get("path", "").split("@")[0]
        == "zephytiju/JumboBuild/.github/workflows/jumbo-verify.yml"
        and ref.get("sha") == producer_sha
        for ref in run.get("referenced_workflows", [])
    )
    verify = [j for j in jobs if j["name"] == "verify / verify"]
    status, reason, measured = "unknown", "unapproved producer or missing verification job", []
    if approved and run.get("conclusion") not in {None, "success"}:
        status, reason = "error", "verification job unavailable or unsuccessful"
    if approved and len(verify) == 1:
        job = verify[0]
        measured = [s for s in job["steps"] if s["name"] in TEST_STEPS
                    and s["conclusion"] != "skipped"]
        if any(s["conclusion"] == "failure" for s in measured):
            status, reason = "failed", "test command failed"
        elif job["conclusion"] != "success":
            status, reason = "error", "verification did not complete successfully"
        elif len(measured) == 1 and measured[0]["conclusion"] == "success":
            status, reason = "passed", "test command succeeded"
        elif not measured:
            status, reason = "not-run", "no test command executed"
    completed = [iso(j["completed_at"]) for j in jobs if j.get("completed_at")]
    # GitHub startup failures may have no jobs at all. Record the completed
    # run's API update time with explicit provenance rather than retaining a
    # stale green badge or pretending a test command ran.
    completed_at = (max(completed, key=lambda s: datetime.fromisoformat(s.replace("Z", "+00:00")))
                    if completed else iso(run["updated_at"]))
    return {
        "schema": "jumbo.test-status/v1", "repository": repo,
        "branch": branch, "commit": run["head_sha"], "workflow": workflow,
        "runId": run_id, "runAttempt": attempt,
        "runUrl": f"https://github.com/{repo}/actions/runs/{run_id}/attempts/{attempt}",
        "completedAt": completed_at,
        "completedAtSource": "job.completed_at" if completed else "run.updated_at",
        "producerCommit": producer_sha,
        "outcome": status, "reason": reason,
        "measurements": [{k: s.get(k) for k in ("name", "conclusion", "started_at", "completed_at")}
                         for s in measured],
    }


def collect(source: dict, producer_sha: str) -> dict | None:
    repo, workflow = coordinates(source)
    branch = api(f"repos/{repo}")["default_branch"]
    component(branch)
    # GitHub orders runs by creation. Select the newest completed push, then
    # fetch its exact attempt; dispatches/releases and PR runs are excluded.
    query = urlencode({"event": "push", "branch": branch, "status": "completed", "per_page": 100})
    runs = api(f"repos/{repo}/actions/workflows/{workflow}/runs?{query}")["workflow_runs"]
    if not runs:
        return None
    run = max(runs, key=lambda r: (r["id"], r["run_attempt"]))
    jobs = pages(f"repos/{repo}/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs", "jobs")
    return result_from_run(source, branch, run, jobs, producer_sha)


def svg(label: str, value: str, color: str) -> str:
    left, right = max(45, len(label) * 7 + 12), max(45, len(value) * 7 + 12)
    title = escape(f"{label}: {value}", quote=True)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{left + right}" height="20" '
            f'role="img" aria-label="{title}"><title>{title}</title>'
            f'<rect width="{left}" height="20" fill="#555"/>'
            f'<rect x="{left}" width="{right}" height="20" fill="{color}"/>'
            '<g fill="#fff" text-anchor="middle" font-family="Verdana,sans-serif" font-size="11">'
            f'<text x="{left / 2}" y="14">{escape(label)}</text>'
            f'<text x="{left + right / 2}" y="14">{escape(value)}</text></g></svg>\n')


def write_result(root: Path, result: dict) -> bool:
    root = root.resolve()
    owner, repo = result["repository"].split("/")
    directory = root / "badges" / component(owner) / component(repo) / component(result["branch"])
    # No symlink may escape the mutable badge subtree, even in a local clone.
    destinations = [directory / name for name in ("status.json", "tests.svg", "last-run.svg")]
    parents = [directory, *list(directory.parents)[:len(directory.relative_to(root).parts) - 1]]
    if any(p.is_symlink() for p in destinations + parents):
        raise ValueError("symlink in badge destination")
    record = directory / "status.json"
    if record.exists():
        old = json.loads(record.read_text())
        if (old["runId"], old["runAttempt"]) >= (result["runId"], result["runAttempt"]):
            return False
    directory.mkdir(parents=True, exist_ok=True)
    label, color = STATUSES[result["outcome"]]
    (directory / "tests.svg").write_text(svg("tests", label, color))
    # Only this measured companion is emitted; no coverage/count estimates.
    (directory / "last-run.svg").write_text(svg("last run", result["completedAt"], "#007ec6"))
    record.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n")
    return True


def publish(index: Path, results: list[dict], branch: str = "main", retries: int = 5) -> bool:
    # Use a disposable worktree. Never reset the caller's branch or stage any
    # package records. A rejected push is retried against the newly fetched tip.
    if command("git", "status", "--porcelain", cwd=index):
        raise ValueError("publication requires a clean index clone")
    for _ in range(retries):
        command("git", "fetch", "origin", branch, cwd=index)
        with tempfile.TemporaryDirectory(prefix="jumbo-badges-") as temp:
            tree = Path(temp) / "index"
            command("git", "worktree", "add", "--detach", str(tree), "FETCH_HEAD", cwd=index)
            try:
                changed = [write_result(tree, result) for result in results]
                if not any(changed):
                    return False
                command("git", "add", "--", "badges/", cwd=tree)
                command("git", "commit", "-m", "badges: update default-branch test status", cwd=tree)
                pushed = subprocess.run(["git", "push", "origin", f"HEAD:refs/heads/{branch}"], cwd=tree)
                if pushed.returncode == 0:
                    return True
            finally:
                command("git", "worktree", "remove", "--force", str(tree), cwd=index)
    raise RuntimeError("badge push failed after bounded fast-forward retries")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--index", type=Path, required=True)
    parser.add_argument("--sources", type=Path, required=True)
    parser.add_argument("--producer-commit", required=True)
    parser.add_argument("--publish", action="store_true", help="fast-forward push from a disposable worktree")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.producer_commit):
        parser.error("producer-commit must be a full SHA")
    results = [r for source in json.loads(args.sources.read_text())
               if (r := collect(source, args.producer_commit)) is not None]
    if args.publish:
        publish(args.index.resolve(), results)
    else:
        for result in results:
            write_result(args.index.resolve(), result)
    print(json.dumps({"results": len(results), "published": args.publish}))


if __name__ == "__main__":
    main()
