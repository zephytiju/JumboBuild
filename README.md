# Jumbo Build

[简体中文](./README.zh-CN.md)

Jumbo Build is Juntai's unified command-line interface for building projects and managing multi-repository workspaces. It gives every supported language the same build, test, format, release-check, and cleanup workflow while keeping language-specific behavior behind a small plugin interface.

## Why Jumbo Build?

- **One project workflow:** use the same commands across every supported language.
- **Multi-repository workspaces:** clone, import, remove, synchronize, and watch repositories under one workspace.
- **Local dependency wiring:** build compatible local Node and Python packages in dependency order, across public/private repositories and any remote organization. Python uses uv workspace sources; Node uses temporary file sources; missing checkouts retain remote fallback.
- **Native CLI:** ship a single Rust binary for macOS and Linux.
- **Extensible language support:** add another ecosystem by implementing `LanguageSupport` and registering it.

Jumbo Build currently supports Python projects identified by a `pyproject.toml` file and Node.js / TypeScript projects identified by a `package.json` file.

## Requirements

- macOS or Linux on x86-64 or ARM64
- Git
- Rust stable and Cargo (the installer builds Jumbo Build from source)
- For Python projects: [uv](https://docs.astral.sh/uv/); project tools such as pytest and Ruff should be declared in the project's dependency groups
- For Node projects: Node.js ≥ 18 and npm ≥ 9 on the PATH (npm ≥ 7 is the hard floor for lockfileVersion 2/3)

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

Project build commands must be run from inside a registered project. Jumbo detects the current project from `jumbo.toml`, builds its local dependency closure first, and applies the requested test, format, release check, or clean action to that project:

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

| Command | Python pipeline | Node pipeline |
| --- | --- | --- |
| `jumbo` or `jumbo build` | `uv lock --upgrade` → `uv sync` → `uv build` | `npm install --package-lock-only --ignore-scripts` → `npm ci` → `npm run build` (when a `build` script is configured) |
| `jumbo test` | build pipeline → `uv run pytest -v` | build pipeline → `npm test` (when configured; npm's "no test specified" placeholder counts as absent) or `node --test` (when Node test files exist) |
| `jumbo format` | build pipeline → `ruff format .` → `ruff check --fix .` | build pipeline → `npm run format` (when configured) or `npx --no-install prettier --write .` (when Prettier is configured) |
| `jumbo release` | build pipeline → `uv run pytest -v` → `ruff check .` | build pipeline → test step → `npm run format:check` (when configured) or `npx --no-install prettier --check .` |
| `jumbo clean` | Remove caches, coverage output, and build artifacts for the current project | Remove `node_modules/`, build output (`dist`, `build`, `out`), coverage, `.eslintcache`, and `*.tsbuildinfo` |

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

## Node.js project behavior

Node.js / TypeScript projects are detected by a `package.json` file. When a repository carries both a `pyproject.toml` and a `package.json`, it builds as Python — the registry checks Python first so existing Python projects can add a `package.json` for tooling without changing identity.

The pipeline selects its steps from the project's own configuration:

- **Lock and install:** every pipeline refreshes `package-lock.json` with `npm install --package-lock-only --ignore-scripts` — the exact command the fingerprint engine's `jumbo lock` uses, so workspace builds and lock generation produce the same file — and then installs with `npm ci`. Lifecycle scripts never run during the lock-only step; they run during `npm ci`, which is the real install. Because installing always follows a fresh lock step, the pipeline is agnostic to which npm wrote the lock (lockfileVersion 2/3 on npm ≥ 7; legacy v1 locks are re-resolved and upgraded by the same step before anything installs from them).
- **Build:** the project's `build` script runs when configured; packages without one get an install-only build.
- **Tests:** `npm test` runs when a real `test` script is configured; otherwise `node --test` runs when the project carries Node test files (`*.test.js` and friends); otherwise the step is skipped with a note.
- **Formatting:** the project's `format` / `format:check` scripts run when configured; otherwise Prettier runs directly when it is configured (a `prettier` dependency, a `prettier` key in package.json, or a Prettier config file). `npx --no-install` uses the locally installed binary and never downloads anything.
- **Engines:** `engines.node` is enforced before any step runs — an unsatisfied range fails the pipeline immediately with both versions named. Ranges jumbo cannot evaluate statically are deferred to npm's own check.
- **Release:** `jumbo release` is the strict variant: build pipeline → tests → strict format check. It never publishes; artifact publication is the executor's job under the publication contract.

Developer workspace builds choose local repositories by their live manifest package name, ecosystem, and compatible declared version range. Repository folder names, remotes, organizations, and public/private flags do not affect selection. The dependency closure is validated before building; duplicate package identities, incompatible local versions, and dependency cycles fail with named diagnostics. Node manifests temporarily point at the selected checkout with `file:` sources; each producer installs and builds before its consumer. The original manifest bytes are restored on success or failure. Python keeps the existing uv workspace sources and normalizes Jumbo's `name@MAJOR` declarations during the build.

When a checkout is absent, public npm dependencies use normal registry resolution and internal packages use the existing SHA-256-verified Jumbo index materializer. Set `JUMBO_INDEX_PATH`/`JUMBO_INDEX_URL` when public packages are also index-managed, and `JUMBO_ARTIFACT_DIR` for an existing artifact cache. Python retains the registered Git-source fallback. No root npm workspace is generated.

Local workspace locks and their `deps/.jumbo-workspace-inputs.json` provenance marker cannot be used for immutable published fingerprints, promotion, or artifact reuse. Dirty source trees also cannot reuse a published artifact. Regenerate publication inputs with `jumbo lock` from the recorded index before promotion. Pinned reproduction continues to resolve the recorded closure, independent of local checkouts.

npm is the supported package manager; `packageManager` fields selecting other tools are not honored yet. Internal dependencies use the `@juntai/*` scope (legacy `@zephytiju/*` is accepted) and are injected at `file:deps/<slug>` coordinates by `jumbo lock` and the materializer — the standalone publication pipeline runs the plain toolchain against recorded artifacts; developer workspace builds select compatible local checkouts first.

Node repositories register with their npm package identity in `jumbo.toml` (`package = "@juntai/kit"`, `ecosystem = "node"`) and are excluded from the uv workspace, so mixed Python + Node workspaces work unchanged: no root npm workspace is generated, because npm workspaces would centralize `node_modules` at the root and change per-project install semantics that jumbo's file-protocol ingestion model relies on.

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

## Dependency materialization (dedup)

`jumbo dedup` decides build-or-reuse against the index by input fingerprint and, with `--materialize` / `--deps`, pulls the recorded artifacts through the validated github.com-only fetch layer (exact URL, recorded SHA-256 enforced — never a registry protocol):

- a **hit** pulls the matched record's own artifact into the project's `dist/` directory, so the repeated run consumes the recorded bytes with zero source rebuilds;
- `--deps` replaces each internal dependency's `deps/<slug>/` source overlay with its recorded release asset — Python wheels as direct uv wheel sources, Node tarballs via `file:` sources.

**Source fallback for dead artifacts:** on the dependency path, a record whose artifact cannot be used — the download answered a definitive 404/410 (the release asset no longer exists at the recorded URL, e.g. a deleted GitHub release asset), the record has no `artifactUrl`, or it has no `artifactSha256` (the bytes would be unverifiable) — falls back to **fetching the dependency's real repository source at the recorded commit** instead of failing the build: `https://codeload.github.com/<owner>/<repo>/tar.gz/<commit>` (GitHub's tarball host, through the same validated https layer) is unpacked into `deps/<slug>/` with the leading directory stripped, replacing the minimal lock stub `jumbo lock` materialized — the stub exists for resolution only and is never buildable. The repository coordinate is resolved by parsing owner/repo from the record's `artifactUrl` when it is a github.com URL (dead asset URLs still carry it), else from a repo map — a JSON object mapping package name to https clone URL, passed via `--repo-map <PATH>` or `JUMBO_REPO_MAP`; without either, a typed error names the package and both options. The unpacked project must carry the record's package name (normalized; the version may differ — the record's version semantics hold). Tarballs have no recorded sha256, so the provenance (tarball URL + commit) is recorded instead; a stale previously pulled artifact for that dependency is removed, and a re-run skips the fetch when the same source materialization already stands. The decision is recorded in the dedup JSON: `dependencies.materialized` entries carry `"mode": "artifact"` or `"source"` plus, for source, the `reason` and the provenance `url`, and `dependencies.keptSourceOverlays` lists every dependency whose source materialization stands.

Transient failures (5xx, network errors) and integrity failures (digest mismatch, malformed digest) do **not** fall back — they abort the build so real outages and tampering stay visible, and a fallback fetch that itself fails (unresolvable repository, dead tarball URL, name mismatch) aborts too: leaving the unbuildable stub standing would just move the failure into uv/npm. The own-record artifact (`--materialize` on a hit) and pinned reproduction never fall back either: a reuse or reproduction that cannot produce the recorded bytes is a failure, not a degradation.

## Deployment pinning

Every promoted index record is the build record a deployment pins. `jumbo pin` resolves one record — by `--by-build-id`, by `--by-commit` (the newest record of that commit), or as `--latest-of-major` — and emits a `jumbo.deployment-pin/v1` manifest: `buildId` (recorded, or the documented `bootstrap-…` derivation for imported records), `commit`, `version`, the exact digest-pinned `imageRef` when the record published an image digest, the artifact URL + SHA-256, the fingerprint, and the record's index location. Bootstrap records with a null `buildId` get a deterministic derived id; a record without an `imageDigest` fails loudly under `--require-image` instead of pinning an imageless build.

`jumbo build --pinned <buildId>` (alias `jumbo reproduce <buildId>`) reproduces a past build from its record — the record is the lock: it recomputes `sha256(commit + canonical extract)`, aborts on any fingerprint mismatch before touching disk, materializes every internal dependency of the recorded closure at its exact recorded version (SHA-256 enforced through the github.com-only fetch layer), and produces the recorded artifact digest in the output directory.

The reference TypeScript adapter (`pinning-adapter/`, consumed by path, never published) maps the manifest onto the existing vangu Selection and PackageLock fields — `buildId` into the selection contract, the exact image string validated by the same regex the IaC applies. See [docs/pinning.md](./docs/pinning.md) for the authoritative field mapping and error taxonomy.

```bash
jumbo pin consumer --by-build-id consumer-2.4.0-001 --require-image
jumbo pin consumer --by-commit 84d0c0ffee...        # newest record of the commit
jumbo build --pinned consumer-2.4.0-001 --artifact-dir cache/
```

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
- [Member onboarding: declaring, building, and pinning with jumbo](./docs/member-onboarding.md) — the service-owner walkthrough
- [Generated CLI reference](./docs/cli-reference.md)
- [Deployment pinning contract](./docs/pinning.md)
- For complete, version-matched command details, run `jumbo --help` or `jumbo <command> --help`.
- [License](./LICENSE)
