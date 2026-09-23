# Jumbo Build

[简体中文](./README.zh-CN.md)

Jumbo Build is Juntai's unified command-line interface for building projects and managing multi-repository workspaces. It gives every supported language the same build, test, format, release-check, and cleanup workflow while keeping language-specific behavior behind a small plugin interface.

## Why Jumbo Build?

- **One project workflow:** use the same commands across every supported language.
- **Multi-repository workspaces:** clone, import, remove, synchronize, and watch repositories under one workspace.
- **Local dependency wiring:** keep checked-out Python packages connected as uv workspace members and fall back to recorded Git remotes when a package is absent locally.
- **Native CLI:** ship a single Rust binary for macOS and Linux.
- **Extensible language support:** add another ecosystem by implementing `LanguageSupport` and registering it.

Jumbo Build currently supports Python projects identified by a `pyproject.toml` file.

## Requirements

- macOS or Linux on x86-64 or ARM64
- Git
- Rust stable and Cargo (the installer builds Jumbo Build from source)
- For Python projects: [uv](https://docs.astral.sh/uv/); project tools such as pytest and Ruff should be declared in the project's dependency groups

## Install

The installer builds the checked-out source and copies `jumbo` to `~/.local/bin`:

```bash
git clone https://github.com/zephytiju/JumboBuild.git
cd JumboBuild
./install.sh
```

Open a new shell after installation, then verify the binary:

```bash
jumbo --version
jumbo --help
```

## Quick start

Create a workspace and add a repository:

```bash
jumbo workspace create my-workspace
cd my-workspace
jumbo workspace use -r https://github.com/example/example-package.git
```

Project build commands must be run from inside a registered project. Jumbo detects the current project from `jumbo.toml` and operates only on that project:

```bash
cd projects/example-package
jumbo          # update lockfile, sync, and build
jumbo test     # build and test
jumbo format   # build, format, and apply safe lint fixes
jumbo release  # build, test, and run strict lint checks
jumbo clean    # remove this project's generated artifacts
```

`jumbo release` is a release-readiness check; it does not publish an artifact.

## Command reference

### Project commands

| Command | Python pipeline |
| --- | --- |
| `jumbo` or `jumbo build` | `uv lock --upgrade` → `uv sync` → `uv build` |
| `jumbo test` | build pipeline → `uv run pytest -v` |
| `jumbo format` | build pipeline → `ruff format .` → `ruff check --fix .` |
| `jumbo release` | build pipeline → `uv run pytest -v` → `ruff check .` |
| `jumbo clean` | Remove caches, coverage output, and build artifacts for the current project |

The explicit forms `jumbo build test`, `jumbo build format`, `jumbo build release`, and `jumbo build clean` are equivalent to the top-level shortcuts.

### Workspace commands

Run workspace commands anywhere below the workspace root unless a command says otherwise. `workspace` can be abbreviated to `ws`.

| Command | Purpose |
| --- | --- |
| `jumbo workspace create <name>` | Create `<name>/`, `projects/`, `jumbo.toml`, the root `pyproject.toml`, and VS Code workspace configuration |
| `jumbo workspace create <name> --import` | Initialize an existing `<name>/` directory and import folders already under its `projects/` directory |
| `jumbo workspace use -r <url> [-r <url> ...]` | Clone one or more Git repositories into `projects/` and register them |
| `jumbo workspace import` | Register all untracked directories under `projects/` |
| `jumbo workspace import -p <name> [-p <name> ...]` | Register selected directories under `projects/` |
| `jumbo workspace remove -p <name> [-p <name> ...]` | Delete selected project directories and update workspace metadata and IDE configuration |
| `jumbo workspace remove -p <name> --yes` | Remove without prompting, including when uncommitted changes exist |
| `jumbo workspace sync` | Reconcile local projects, metadata, uv sources, and IDE configuration |
| `jumbo workspace watch --interval 30` | Repeat synchronization until interrupted |
| `jumbo workspace clean` | Clean generated artifacts across the workspace and all registered local projects |

Removing a project deletes its directory. Without `--yes`, Jumbo asks for confirmation when it detects uncommitted changes.

## Python dependency behavior

Each Python project remains responsible for declaring its own dependencies in `project.dependencies`. Jumbo maintains shared resolution information at the workspace root:

- local Python repositories become uv workspace members and use `{ workspace = true }` sources;
- registered packages missing from disk can use their recorded Git remotes;
- non-Python directories are excluded from the uv workspace;
- only source entries listed in `[tool.jumbo.workspace_sources]` are managed, so unrelated root configuration is preserved.

`jumbo.toml` records workspace membership, repository paths, remotes, package names, and IDE settings. Treat it as workspace metadata and commit it with the workspace configuration.

## Dependency resolution (Jumbo index)

Internal dependencies are declared **by major version only** and resolved against the [Jumbo index](https://github.com/zephytiju/JumboIndex) — the append-only record of every promoted internal build. Resolution returns the newest index record of the declared major (newest by record order, not timestamp). Third-party dependencies pass through untouched for the normal language tooling.

Accepted declaration forms:

| Manifest | Accepted | Rejected |
| --- | --- | --- |
| `pyproject.toml` | `juntai-fuse-api[http]@2` (jumbo major-only), `pkg==2.*`, `pkg~=2.0`, `pkg>=2,<3` | Git URLs (`git+https://…`), direct wheel/tarball URLs, exact pins (`==2.1.3`), ranges spanning or not bounding a single major (`>=2`, `>=1,<3`) |
| `package.json` (`@juntai/*`, legacy `@zephytiju/*`) | `"1"`, `"1.x"`, `"^1"`, `"~1"`, `">=1,<2"` | Git/tarball URLs, `user/repo` shorthands, `file:` paths, exact pins (`"1.2.3"`), `">=1"`, `">=1,<3"`, `"*"` |

```bash
# Resolve one declaration
jumbo resolve juntai-fuse-api[http]@2
jumbo resolve '@juntai/demo-kit@^1'

# Validate and resolve every dependency of a manifest
jumbo resolve --manifest projects/consumer/pyproject.toml
jumbo resolve --manifest projects/console/package.json

# Validate declaration forms only (no record lookups)
jumbo resolve --manifest pyproject.toml --check
```

The index location is `--index <path-or-url>`, then `JUMBO_INDEX_PATH` (local clone; recommended), then `JUMBO_INDEX_URL`, then the JumboIndex repository (fetched read-only via `gh`; only `https://github.com` URLs are accepted). An internal dependency with no index record fails with an **absorption error** naming the package and the absorption step: its repository must be covered by a jumbo pipeline before it can be consumed.

## Lock generation and fingerprint

`jumbo lock` generates the language lock for a manifest with internal dependencies injected from the index: every internal package is materialized as a minimal source project at a **stable relative path** (`deps/<package-slug>/`) carrying the index record's name and version, the manifest is rewritten to point at the injected sources (Python: `name[extras]==<version>` plus a `[tool.uv.sources]` path entry; npm: `"file:deps/<slug>"`), and the normal language tool produces the lock — `uv lock --upgrade` for `uv.lock`, `npm install --package-lock-only --ignore-scripts` for `package-lock.json`. Third-party ranges are re-resolved on every run. The rewrite is recorded in `deps/.jumbo-sources.json`, is fully reversible, and is idempotent: two runs in a row produce identical files.

```bash
jumbo lock --manifest projects/consumer/pyproject.toml
jumbo lock --manifest projects/console/package.json
jumbo lock --manifest pyproject.toml --inject-only   # injection without uv/npm
```

`jumbo fingerprint` computes the build input fingerprint **sha256(own commit + canonical extract of the generated lock)**. Raw lock bytes are never hashed: the canonical extract is a sorted, tool-independent view of what the build materializes, so a formatting-only lock change (different uv/npm version, key order, entry order) produces the same extract and no rebuild, while any real resolution change produces a different fingerprint. With `--lock` it fingerprints an existing lock file as a pure-local query; without it, it generates the lock first.

```bash
# Pure-local query against an existing lock
jumbo fingerprint --lock projects/consumer/uv.lock

# Generate the lock, then fingerprint it
jumbo fingerprint --manifest projects/consumer/pyproject.toml
```

The report is JSON: `commit`, `ecosystem`, `lock`, `canonicalExtract` (exactly what an index record stores), and `fingerprint`, plus the working-tree state. The canonical extract format is `jumbo-canonical-extract/1`:

```json
{
  "format": "jumbo-canonical-extract/1",
  "entries": [
    { "name": "demo-alpha", "version": "2.4.0", "source": "index", "digest": null, "path": "deps/demo-alpha" },
    { "name": "numpy", "version": "1.26.4", "source": "pypi", "digest": "sha256:…", "path": null }
  ]
}
```

Entries are sorted by (name, version, source, digest, path) and deduplicated; `source` is `index` (jumbo-injected internal package, identified by its stable `deps/<slug>` coordinate), `pypi`/`npm` (third-party registry entry with its integrity digest), or `path` (another local source). The own project is excluded — it is represented by the own commit. The fingerprint preimage is byte-exact `<40-hex commit>\n<canonical JSON>` (compact, struct field order).

**Promotion guard:** promotion happens only on clean commits inside a pipeline; local builds on dirty working trees never promote. `jumbo fingerprint --promote` (and any future promotion-mode operation) refuses unless the working tree is attributable to exactly the HEAD commit. Jumbo-generated output is exempt: everything under `deps/`, the generated lock files, and a manifest whose only difference from HEAD is jumbo's recorded injection rewrite. A modified source file or a stray untracked file refuses promotion with the offending paths listed.

## Shell completion

The installer configures dynamic completion for zsh, Bash, or fish. You can also generate a static completion script:

```bash
jumbo completions zsh > _jumbo
jumbo completions bash > jumbo.bash
jumbo completions fish > jumbo.fish
```

Install the generated file in the location expected by your shell. See `jumbo completions --help` for every supported shell.

## Development and support

- [Development guide](./DEVELOPMENT.md)
- [Generated CLI reference](./docs/cli-reference.md)
- For complete, version-matched command details, run `jumbo --help` or `jumbo <command> --help`.
- [License](./LICENSE)
