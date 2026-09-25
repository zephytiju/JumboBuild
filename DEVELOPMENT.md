# Jumbo Build development guide

[User guide](./README.md) · [Generated CLI reference](./docs/cli-reference.md)

This guide is for contributors changing Jumbo Build itself. It explains the development workflow, the boundaries between modules, and the contracts that should remain stable as the CLI evolves.

## Set up a development environment

### Requirements

- Rust stable and Cargo, preferably installed through [rustup](https://rustup.rs/)
- Git
- macOS or Linux
- uv and a small Python fixture project when exercising the Python backend end to end
- Node.js ≥ 18 and npm ≥ 9 when exercising the Node backend end to end (the offline CLI fixtures carry zero third-party dependencies)

### Build and verify

```bash
git clone https://github.com/zephytiju/JumboBuild.git
cd JumboBuild

cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets --all-features
cargo run -- --help
```

Build an optimized binary with `cargo build --release`. The result is `target/release/jumbo`.

### Exercise the CLI locally

Use `cargo run --` in place of `jumbo` while developing:

```bash
cargo run -- workspace --help
cargo run -- completions zsh
```

Project commands require a real Jumbo workspace and must be launched from a registered project directory. For an end-to-end check, build the binary, create or reuse a fixture workspace, register a Python project, and run the binary from that project's directory. Do not use a production workspace as a destructive `remove` or `clean` fixture.

## Architecture

Jumbo separates command parsing, workspace coordination, language behavior, and process execution. Preserve these boundaries when adding functionality:

```text
src/
├── lib.rs                  # Shared module root used by the CLI and doc generator
├── main.rs                 # Startup, dynamic completion, and top-level dispatch
├── cli/
│   ├── mod.rs              # clap command and argument model
│   ├── build.rs            # Current-project build command dispatch
│   ├── resolve.rs          # Dependency-resolution command dispatch
│   ├── lock.rs             # Lock-generation command dispatch
│   ├── fingerprint.rs      # Fingerprint command dispatch
│   └── workspace.rs        # Workspace subcommand dispatch
├── resolver/
│   ├── mod.rs              # Resolution orchestration: internal/external split, reports
│   ├── declaration.rs      # Major-pinned declaration parsing (Python + npm grammars)
│   ├── error.rs            # Typed resolver errors (absorption, not-major-only, forbidden references)
│   ├── index.rs            # JumboIndex reader: local paths and gh-based GitHub fetch
│   └── manifest.rs         # pyproject.toml / package.json loading and form validation
├── fingerprint/
│   ├── mod.rs              # Fingerprint orchestration and report model
│   ├── extract.rs          # Canonical extract + uv.lock/package-lock.json parsers
│   ├── lockgen.rs          # Lock generation: deps/<slug> injection, manifest rewrite, marker
│   ├── gitguard.rs         # Own-commit discovery and the clean-tree promotion guard
│   └── error.rs            # Typed fingerprint-engine errors
├── workspace/
│   ├── mod.rs              # Workspace lifecycle and configuration reconciliation
│   ├── detection.rs        # Upward search for the workspace root
│   ├── metadata.rs         # jumbo.toml data model and persistence
│   └── vscode.rs           # VS Code workspace generation
├── language/
│   ├── mod.rs              # LanguageSupport contract, registry, and detection
│   ├── python.rs           # uv, pytest, Ruff, and Python cleanup behavior
│   └── node.rs             # npm, Node test runner, Prettier, and Node cleanup behavior
└── utils/
    └── runner.rs           # Child-process execution and build result reporting
```

### Request flow

1. `main.rs` lets clap parse arguments and dispatches the selected top-level command.
2. `cli/` validates command context and delegates the operation.
3. Workspace operations discover the root, load `jumbo.toml`, and reconcile generated configuration.
4. Project build operations identify the registered repository containing the current directory, detect its language, and call one `LanguageSupport` implementation.
5. The language implementation describes the external tool steps; `utils::runner` executes them and reports the result.

The CLI model in `src/cli/` is the source of truth for commands, arguments, aliases, defaults, and help text. User documentation should explain workflows and intent; it should not become a second hand-maintained command schema.

## Core contracts

### Workspace layout

A Jumbo workspace has this conceptual shape:

```text
my-workspace/
├── jumbo.toml
├── pyproject.toml
├── my-workspace.code-workspace
└── projects/
    ├── PackageA/
    └── ServiceB/
```

- `jumbo.toml` records repository identity, location, optional remote and package name, and IDE settings.
- The root `pyproject.toml` is the uv workspace configuration. Jumbo creates the minimum required project metadata and preserves unrelated user settings.
- Repositories live under `projects/`; paths stored in metadata are relative to the workspace root.
- Project commands act on the registered repository containing the current working directory. Workspace commands may coordinate every repository.

Example metadata:

```toml
[workspace]
name = "my-workspace"

[[workspace.repositories]]
name = "PackageA"
path = "projects/PackageA"
remote = "https://github.com/example/PackageA.git"
package = "package-a"

[workspace.ide]
type = "vscode"
git_auto_repo_detection = true
git_repo_scan_max_depth = 2
```

### Managed Python configuration

The Python backend updates uv workspace membership and sources in one pass for all registered repositories. It records the package names it owns in `[tool.jumbo.workspace_sources]`, removes only previously managed source entries, and preserves everything else. Changes to this logic must remain idempotent: running `jumbo workspace sync` twice without filesystem changes should not change the generated files the second time.

### Managed Node configuration

Node projects need no workspace-root configuration: internal dependencies resolve through the Jumbo index (`jumbo lock` injects `file:deps/<slug>` sources; the materializer ingests release assets the same way), so `NodeSupport::sync_workspace` is a deliberate no-op and no root npm workspace is generated. `jumbo.toml` records each Node repository's npm package identity (`package` plus `ecosystem = "node"`); the Python backend's git-source fallback is gated on the recorded ecosystem so npm names never enter the uv workspace, while legacy metadata without the field stays Python by construction.

### Resolver core contract

The resolver (`src/resolver/`) implements the Jumbo Build & Versioning Standard's resolution semantics and nothing more — fingerprinting, materialization, and version bumping are explicitly out of scope:

- Declarations reduce to a major: Python accepts `name[extras]@MAJOR`, `==M.*`, `~=M.0`, and `>=M,<M+1`; npm accepts `M`, `M.x`, `^M`, `~M`, and `>=M,<M+1`. Exact pins, floors above `M.0`, unbounded ranges, and multi-major ranges are rejected for internal dependencies.
- Git URLs, direct artifact URLs, and local path references are rejected for every declaration in every validated manifest section; third-party ranges pass through untouched.
- Internal = packages with an index record, the `@juntai/*` and legacy `@zephytiju/*` npm scopes, or the jumbo `name@MAJOR` syntax. An internal dependency without any index record is an absorption error that names the package and the absorption step.
- Resolution returns the newest record of the declared major by record order (last matching JSONL line), never by wall-clock timestamp; bootstrap records are valid targets.
- The index is read from `--index`, `JUMBO_INDEX_PATH`, `JUMBO_INDEX_URL`, or the JumboIndex repository fetched read-only via `gh`. URL sources must be `https://github.com/<owner>/<repo>` exactly; no credentials are read, stored, or embedded — `gh` supplies authentication from its own environment.

### Fingerprint engine contract

The fingerprint engine (`src/fingerprint/`) implements the standard's duplicate-detection input: `sha256(own commit + canonical extract of the generated language lock)`. Dedup decisions against index history, artifact download, and version bumping are explicitly out of scope.

- Lock generation injects every internal dependency as a minimal source project at the stable relative path `deps/<slug>/` (name and version from the resolved index record, commit recorded for provenance) and rewrites the manifest — Python `name[extras]==<version>` plus a `[tool.uv.sources]` path entry, npm `"file:deps/<slug>"` — before the normal tool runs (`uv lock --upgrade`, `npm install --package-lock-only --ignore-scripts`).
- The rewrite lives in `deps/.jumbo-sources.json` (`jumbo-lock-injection/1`), restores before every re-resolution (the resolver never sees rewritten forms), and is idempotent: two generations produce identical files.
- The canonical extract (`jumbo-canonical-extract/1`, matching the JumboIndex record schema) is a sorted, deduplicated, tool-independent view: entries of `name`, `version`, `source` (`index`/`pypi`/`npm`/`path`), `digest`, `path`. Lock formatting, key order, and entry order never affect it; injected coordinates are always `deps/<slug>`; absolute paths and Git URLs in a lock are hard errors.
- The fingerprint preimage is byte-exact `<40-hex commit>\n<compact canonical JSON>`; changing it invalidates every recorded fingerprint.
- Promotion mode (`--promote`) refuses dirty trees: only jumbo-generated output (under `deps/`, generated lock files, and the recorded manifest rewrite verified by un-injecting both sides against HEAD) is exempt; pure-local queries never inspect promotion state.

### Dedup decision and materialization contract

The dedup decision (`src/dedup/`) compares the input fingerprint against the index history of the same package name: a hit reuses the matched record's artifact, a miss builds from source. Materialization pulls release assets by exact URL through the github.com-only fetch layer (`src/dedup/fetch.rs`: https only, github.com plus GitHub's release-asset CDN hosts, redirects re-validated hop by hop, credentials from the environment or `gh` handed to curl via a mode-0600 config — never on the command line) and verifies the recorded SHA-256 before anything on disk is mutated: every artifact stages first, the manifest/overlay rewrite happens only afterwards.

- The own-record artifact lands in `dist/`; dependency artifacts replace the J3 `deps/<slug>/` source overlays (wheels as direct uv sources, tarballs as `file:` sources) and are recorded in `deps/.jumbo-artifacts.json` (`jumbo-artifact-materialization/1`).
- **Dependency source fallback**: a 404/410 download (`MaterializeError::ArtifactGone` — the recorded asset is definitively gone) or a null `artifactSha256` (bytes would be unverifiable) degrades that dependency to its source overlay — the J3 lock path at the recorded commit — instead of failing the build. Entries carry `mode` (`artifact`/`source`) and a `reason`; `keptSourceOverlays` reports every standing overlay; a previously pulled artifact for a fallen-back dependency is removed so the overlay is its single materialization.
- 5xx and network errors, digest mismatches, malformed digests, and egress-policy violations abort (no fallback), and the own-record artifact plus pinned reproduction never fall back: a reuse or reproduction that cannot produce the recorded bytes is a failure.
- Offline tests inject the transport: the fetch layer drives `curl` from `PATH`, so a PATH shim answering fixture statuses and bytes (200/404/410/500) exercises the whole classification with no network and no credentials — see `tests/dedup_cli.rs`.

### Failure behavior

External commands run sequentially and stop the pipeline on the first non-zero exit status. A project pipeline exits the process after printing its final success or failure banner. Workspace operations return structured `anyhow::Result` errors to `main`.

## Make a change

### Add or change a command

1. Define the clap command, argument, help text, aliases, and defaults in `src/cli/`.
2. Keep dispatch thin; put workspace state transitions in `workspace/` and language-specific behavior in `language/`.
3. Update or add tests for parsing and behavior.
4. Check `cargo run -- <command> --help` and all affected parent help pages.
5. Update the user guides only when the workflow or conceptual behavior changed.

Help text should be specific enough to stand on its own because it is also the best version-matched reference for users and coding agents.

### Add a language backend

1. Add `src/language/<language>.rs` and implement every method of `LanguageSupport`:

```rust
pub trait LanguageSupport: Send + Sync {
    fn name(&self) -> &str;
    fn detect(&self, repo_path: &Path) -> bool;
    fn sync_workspace(&self, workspace_root: &Path, repos: &[RepoInfo]) -> Result<()>;
    fn build(&self, workspace_root: &Path, repo_path: &Path) -> Result<()>;
    fn test(&self, workspace_root: &Path, repo_path: &Path) -> Result<()>;
    fn format(&self, workspace_root: &Path, repo_path: &Path) -> Result<()>;
    fn release(&self, workspace_root: &Path, repo_path: &Path) -> Result<()>;
    fn clean(&self, repo_path: &Path) -> Result<()>;
}
```

2. Export the module and register one instance in `get_registry()` in `src/language/mod.rs`.
3. Make `detect()` narrow enough not to capture another language's projects. The first matching backend wins.
4. Keep workspace synchronization idempotent and preserve user-owned configuration.
5. Define what build, test, format, release check, and cleanup mean for the ecosystem, then document any new prerequisite in both user guides.
6. Add detection, synchronization, pipeline, cleanup, and failure-path tests.

## Verification strategy

Run checks proportional to the change. The baseline before opening a pull request is:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test
cargo build --release
```

For CLI changes, also inspect generated help and verify both the shortcut and explicit command form where applicable. For workspace changes, use a temporary workspace covering local repositories, absent repositories with remotes, non-language directories, and a second sync to test idempotence. For process changes, verify that failing child commands produce a non-zero exit code.

## Contribution conventions

- Use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) such as `feat:`, `fix:`, `docs:`, `refactor:`, and `chore:`.
- Keep CLI parsing, workspace state, language integrations, and command execution separate.
- Add context to I/O and external-tool errors so the failing path or operation is visible.
- Let `cargo fmt` decide formatting and resolve all relevant Clippy warnings.
- Never test destructive workspace behavior against a developer's only local checkout.

## Documentation maintenance

- Keep `README.md` and `README.zh-CN.md` aligned in meaning, not necessarily word for word.
- Keep this English development guide as the single contributor reference.
- Describe design intent and ownership boundaries here; keep exact signatures next to the Rust code unless a signature defines an extension contract.
- Generate command reference material from clap rather than manually copying command trees.
- Run `cargo run --example generate-cli-docs` after changing the clap model and commit the updated `docs/cli-reference.md`.
- Update the architecture index only when ownership or top-level structure changes.

## Module index

| Concern | Primary location | Responsibility |
| --- | --- | --- |
| Shared crate root | `src/lib.rs` | Expose the CLI model to the binary and documentation generator |
| CLI schema and help | `src/cli/mod.rs`, `src/cli/*.rs` | Commands, arguments, defaults, aliases, dispatch |
| Current-project pipelines | `src/cli/build.rs` | Repository selection and language pipeline choice |
| Resolver command | `src/cli/resolve.rs` | Declaration/manifest resolution dispatch |
| Lock command | `src/cli/lock.rs` | Lock-generation dispatch (injection + language tool) |
| Fingerprint command | `src/cli/fingerprint.rs` | Fingerprint report dispatch (generate or read a lock) |
| Resolution semantics | `src/resolver/mod.rs` | Internal/external split, newest-of-major, absorption, reports |
| Declarations | `src/resolver/declaration.rs` | Major-pinned syntax for pyproject/package.json, forbidden references |
| Index reader | `src/resolver/index.rs` | JSONL records, local index paths, validated GitHub fetch via `gh` |
| Manifest loading | `src/resolver/manifest.rs` | Read dependency lists from pyproject.toml and package.json |
| Fingerprint orchestration | `src/fingerprint/mod.rs` | Report model, generate/read-and-fingerprint flows |
| Canonical extract | `src/fingerprint/extract.rs` | jumbo-canonical-extract/1, uv/npm lock parsers, sha256 preimage |
| Lock generation | `src/fingerprint/lockgen.rs` | deps/<slug> injection, manifest rewrite/restore, marker |
| Promotion guard | `src/fingerprint/gitguard.rs` | Own-commit discovery, clean-tree enforcement with jumbo exemptions |
| Fingerprint errors | `src/fingerprint/error.rs` | Typed, actionable fingerprint-engine errors |
| Resolver errors | `src/resolver/error.rs` | Typed, actionable error messages for resolution failures |
| Workspace lifecycle | `src/workspace/mod.rs` | Create, clone, import, remove, sync, watch, and clean |
| Workspace discovery | `src/workspace/detection.rs` | Find and validate workspace context |
| Metadata | `src/workspace/metadata.rs` | Parse and write `jumbo.toml` |
| IDE integration | `src/workspace/vscode.rs` | Generate VS Code workspace settings |
| Language contract | `src/language/mod.rs` | Backend interface, registry, and detection order |
| Python backend | `src/language/python.rs` | uv configuration and Python project tool pipelines |
| Node backend | `src/language/node.rs` | npm pipelines, engines enforcement, Prettier selection, Node cleanup |
| Process runner | `src/utils/runner.rs` | Execute sequential shell steps and finalize builds |
