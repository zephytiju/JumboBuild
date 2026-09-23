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
