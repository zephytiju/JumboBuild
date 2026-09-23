//! `jumbo promote` — compute the auto-promotion decision for one package
//! build (Jumbo Build & Versioning Standard, §2.1, §3.5).
//!
//! The command is the executor-facing entry point of the version bump
//! engine: it computes (or consumes) the input fingerprint, reads the
//! index history, and emits the promotion decision JSON — the
//! publish-on-bump contract CircleCI jobs and the `jumbo-publish` GitHub
//! Actions workflow consume. It never publishes and never writes the
//! index.
//!
//! Pipeline order: `jumbo lock --refresh=…` (generates the lock under the
//! refresh gate) → `jumbo promote` (decision). Promote consumes the
//! existing lock next to the manifest and only generates one when absent,
//! applying the same refresh gate to its own lock-tool invocation.
//!
//! Promotion only happens on clean commits inside a pipeline: promote
//! enforces the same clean-tree guard as `jumbo fingerprint --promote`
//! and refuses a dirty working tree before computing anything.

use anyhow::{bail, Context, Result};
use clap::Args;
use std::path::PathBuf;
use std::str::FromStr;

use crate::fingerprint::{self, FingerprintReport};
use crate::promotion::{self, PromotionInputs};
use crate::resolver;

/// Compute the auto-promotion decision: minor on own-source change, patch
/// on dependency-closure change, M.0.0 bootstrap on a new major, none on a
/// duplicate fingerprint; publish-on-bump contract included
#[derive(Args)]
pub struct PromoteArgs {
    /// Manifest of the package about to promote (pyproject.toml or
    /// package.json); its own version declares the major. Default: the
    /// manifest in the current directory
    #[arg(short, long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,

    /// Consume an existing lock file (uv.lock or package-lock.json)
    /// instead of the manifest's; the sibling manifest still provides the
    /// own name and declared major
    #[arg(short, long, value_name = "PATH")]
    pub lock: Option<PathBuf>,

    /// Package name whose index history decides (default: the manifest's
    /// own name)
    #[arg(short, long, value_name = "NAME")]
    pub package: Option<String>,

    /// Jumbo index location: a local clone path or an https://github.com URL
    /// (default: JUMBO_INDEX_PATH, then JUMBO_INDEX_URL, then the JumboIndex repository)
    #[arg(short, long, value_name = "PATH_OR_URL")]
    pub index: Option<String>,

    /// Third-party refresh policy gating the lock tool's --upgrade step:
    /// run (default) or schedule:<interval|cron> (e.g. 24h, 7d, 0 3 * * *)
    /// (default: JUMBO_REFRESH, then run)
    #[arg(short = 'r', long, value_name = "POLICY")]
    pub refresh: Option<String>,
}

pub fn execute(args: PromoteArgs) -> Result<()> {
    if args.lock.is_some() && args.manifest.is_some() {
        bail!("--lock and --manifest cannot be combined");
    }

    // 1. Locate the manifest (always required: the own version declares
    //    the developer-owned major) and the lock to fingerprint.
    let (manifest, lock) = match (&args.manifest, &args.lock) {
        (Some(manifest), None) => {
            if !manifest.exists() {
                bail!("manifest not found: {}", manifest.display());
            }
            (manifest.clone(), None)
        }
        (None, Some(lock)) => {
            if !lock.exists() {
                bail!("lock file not found: {}", lock.display());
            }
            let sibling = lock
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| {
                    p.join(
                        if lock.file_name().and_then(|n| n.to_str()) == Some("uv.lock") {
                            "pyproject.toml"
                        } else {
                            "package.json"
                        },
                    )
                })
                .unwrap_or_else(|| PathBuf::from("pyproject.toml"));
            if !sibling.exists() {
                bail!(
                    "promote needs the package manifest (the declared major): no pyproject.toml \
                     or package.json next to {}",
                    lock.display()
                );
            }
            (sibling, Some(lock.clone()))
        }
        (None, None) => (detect_manifest()?, None),
        _ => unreachable!("both set was rejected above"),
    };

    // 2. Index + the package identity (name and declared major).
    let source = resolver::resolve_source(args.index.as_deref())?;
    let index = resolver::Index::load(&source)
        .map_err(|e| anyhow::anyhow!(e).context(format!("index source: {}", source.describe())))?;
    let package = match &args.package {
        Some(name) => name.clone(),
        None => super::dedup::manifest_package_name(&manifest)?,
    };
    let declared_major = manifest_declared_major(&manifest)?;

    // 3. The refresh gate, evaluated against the newest record of the
    //    declared major before anything runs (the gate also applies to the
    //    lock generation below when no lock exists yet).
    let policy = promotion::resolve_policy(args.refresh.as_deref())?;
    let newest_timestamp = index
        .newest_of_major(&package, declared_major)
        .map(|(_, record)| record.timestamp.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let gate = promotion::evaluate(&policy, newest_timestamp.as_deref(), now);

    // 4. The input fingerprint. Promotion mode: the clean-tree guard is
    //    enforced — local builds on dirty working trees never promote.
    //    An existing lock (the pipeline's `jumbo lock` output) is consumed
    //    as-is; only when absent does promote generate one under the gate.
    let mut lock_tool_ran = false;
    let report = match &lock {
        Some(lock) => fingerprint::fingerprint_lock_file(lock, true).map_err(|e| {
            anyhow::anyhow!(e).context(format!("fingerprinting {}", lock.display()))
        })?,
        None => {
            let lock_name = match manifest.file_name().and_then(|n| n.to_str()) {
                Some("pyproject.toml") => "uv.lock",
                _ => "package-lock.json",
            };
            let existing = manifest
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.join(lock_name))
                .filter(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from(lock_name));
            if existing.is_file() {
                fingerprint::fingerprint_lock_file(&existing, true).map_err(|e| {
                    anyhow::anyhow!(e).context(format!("fingerprinting {}", existing.display()))
                })?
            } else {
                lock_tool_ran = true;
                let (_generation, report) =
                    fingerprint::fingerprint_manifest(&manifest, &index, true, true, gate.upgrade)
                        .map_err(|e| {
                            anyhow::anyhow!(e)
                                .context(format!("fingerprinting {}", manifest.display()))
                        })?;
                report
            }
        }
    };
    assert_promotion_clean(&report)?;

    // 5. The decision — derived only from the own commit, the canonical
    //    extract, and the index history.
    let inputs = PromotionInputs {
        package: &package,
        ecosystem: report.ecosystem,
        declared_major,
        commit: &report.commit,
        extract: &report.canonical_extract,
        fingerprint: &report.fingerprint,
    };
    let decision = promotion::decide(&inputs, &index, gate, lock_tool_ran)
        .map_err(|e| anyhow::anyhow!(e).context("promotion decision"))?;

    println!("{}", serde_json::to_string_pretty(&decision)?);
    Ok(())
}

/// The manifest's declared major: the major segment of its own version
/// (`[project].version` / package.json `version`). The developer owns the
/// major; the minor/patch segments in the manifest are advisory and
/// ignored (the pipeline owns them).
pub(crate) fn manifest_declared_major(manifest: &std::path::Path) -> Result<u64> {
    let is_python = manifest
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "pyproject.toml");
    let version = if is_python {
        let content =
            std::fs::read_to_string(manifest).context(format!("reading {}", manifest.display()))?;
        let doc: toml::Table = content
            .parse()
            .with_context(|| format!("parsing {}", manifest.display()))?;
        doc.get("project")
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .with_context(|| {
                format!(
                    "{} has no [project].version; the developer declares the major there",
                    manifest.display()
                )
            })?
    } else {
        let content =
            std::fs::read_to_string(manifest).context(format!("reading {}", manifest.display()))?;
        let doc: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("parsing {}", manifest.display()))?;
        doc.get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .with_context(|| {
                format!(
                    "{} has no version; the developer declares the major there",
                    manifest.display()
                )
            })?
    };
    let parsed = crate::promotion::JumboVersion::from_str(&version).map_err(|e| {
        anyhow::anyhow!(e).context(format!(
            "{}: the manifest own version must be `major.minor.patch`",
            manifest.display()
        ))
    })?;
    Ok(parsed.major)
}

/// After fingerprinting, promotion still requires an attributable tree —
/// a guard failure already aborts earlier, this double-checks the state
/// the fingerprint engine reported.
fn assert_promotion_clean(report: &FingerprintReport) -> Result<()> {
    if report.promotion && !report.tree_clean {
        bail!(
            "promotion refused: the working tree is not attributable to commit {} (dirty: {})",
            report.commit,
            report.dirty_paths.join(", ")
        );
    }
    Ok(())
}

/// Detect pyproject.toml or package.json in the current directory.
fn detect_manifest() -> Result<PathBuf> {
    for name in ["pyproject.toml", "package.json"] {
        let candidate = PathBuf::from(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("no pyproject.toml or package.json in the current directory; pass --manifest <PATH> or --lock <PATH>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_major_comes_from_the_manifest_own_version() {
        let dir = std::env::temp_dir().join(format!(
            "jumbo-promote-cli-ut-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let pyproject = dir.join("pyproject.toml");
        std::fs::write(
            &pyproject,
            "[project]\nname = \"consumer\"\nversion = \"3.1.4\"\ndependencies = []\n",
        )
        .expect("pyproject");
        assert_eq!(manifest_declared_major(&pyproject).unwrap(), 3);
        // Minor/patch in the manifest are advisory: the major is the only
        // input.
        std::fs::write(&pyproject, "[project]\nname = \"c\"\nversion = \"0.0.1\"\n").unwrap();
        assert_eq!(manifest_declared_major(&pyproject).unwrap(), 0);

        let package_json = dir.join("package.json");
        std::fs::write(
            &package_json,
            r#"{"name": "@juntai/c", "version": "2.9.3"}"#,
        )
        .expect("package.json");
        assert_eq!(manifest_declared_major(&package_json).unwrap(), 2);

        // Missing or malformed own versions are actionable errors.
        std::fs::write(&pyproject, "[project]\nname = \"c\"\n").unwrap();
        assert!(manifest_declared_major(&pyproject)
            .unwrap_err()
            .to_string()
            .contains("[project].version"));
        std::fs::write(&package_json, r#"{"name": "@juntai/c", "version": "2"}"#).unwrap();
        assert!(manifest_declared_major(&package_json)
            .unwrap_err()
            .to_string()
            .contains("major.minor.patch"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
