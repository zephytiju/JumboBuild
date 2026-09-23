//! Major-pinned dependency declarations.
//!
//! A declaration is one dependency entry from a manifest (`pyproject.toml`
//! dependency lists or `package.json` dependency maps). The parser classifies
//! its version part into one of three specs:
//!
//! - [`Spec::Major`] — the jumbo major-only form `name[extras]@MAJOR`;
//! - [`Spec::Range`] — a range that collapses to exactly one major
//!   (`==2.*`, `~=2.0`, `>=2,<3` for Python; `1`, `1.x`, `^1`, `~1`,
//!   `>=1,<2` for npm);
//! - [`Spec::Other`] — anything else. Third-party dependencies pass through
//!   with this spec untouched; internal dependencies with this spec are a
//!   manifest violation (`NotMajorOnly`).
//!
//! Git URLs, direct artifact URLs, and local path references are forbidden
//! for every declaration and fail at parse time: no manifest ever carries a
//! GitHub URL (Jumbo Build & Versioning Standard, Resolution Semantics).

// The resolver error enum carries rich context strings for actionable
// messages and flows through `anyhow` at the CLI boundary, where its
// stack size is not performance-relevant.
#![allow(clippy::result_large_err)]
use super::error::{NotMajorOnlyReason, ResolverError};
use super::index::normalize_python_name;

/// Accepted major-only forms, used in error guidance.
pub const PYTHON_ACCEPTED_FORMS: &str =
    "`name[extras]@MAJOR` (e.g. `juntai-fuse-api[http]@2`), `name==MAJOR.*`, `name~=MAJOR.0`, or `name>=MAJOR,<MAJOR+1`";
/// Accepted major-only forms, used in error guidance.
pub const NPM_ACCEPTED_FORMS: &str =
    "\"MAJOR\" (e.g. \"2\"), \"MAJOR.x\", \"^MAJOR\", \"~MAJOR\", or \">=MAJOR,<MAJOR+1\"";

/// The version part of a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// The jumbo major-only form `name[extras]@MAJOR`.
    Major(u64),
    /// A range collapsing to exactly one major.
    Range { major: u64, text: String },
    /// Anything else; carries the reason it is not major-only.
    Other {
        text: String,
        reason: NotMajorOnlyReason,
    },
}

impl Spec {
    /// The single major this spec collapses to, if any.
    pub fn major(&self) -> Option<u64> {
        match self {
            Self::Major(major) | Self::Range { major, .. } => Some(*major),
            Self::Other { .. } => None,
        }
    }

    /// Why this spec is not major-only (only meaningful for `Other`).
    pub fn not_major_only_reason(&self) -> NotMajorOnlyReason {
        match self {
            Self::Other { reason, .. } => reason.clone(),
            _ => NotMajorOnlyReason::NoSpecifier,
        }
    }
}

/// One parsed dependency declaration from a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// The original declaration text (requirement string or version value).
    pub raw: String,
    /// Normalized package name (PEP 503 for Python, as-written for npm).
    pub name: String,
    /// Python extras (`[http,async]`), sorted and deduplicated.
    pub extras: Vec<String>,
    /// Python environment marker (`; python_version >= "3.11"`), kept verbatim.
    pub marker: Option<String>,
    /// The version part.
    pub spec: Spec,
    /// Where the declaration appeared, for error messages.
    pub location: String,
}

/// Parse one Python dependency declaration (PEP 508 subset plus the jumbo
/// `name[extras]@MAJOR` form).
pub fn parse_python(raw: &str, location: &str) -> Result<Declaration, ResolverError> {
    let raw_trim = raw.trim();
    let invalid = |reason: String| ResolverError::InvalidDeclaration {
        raw: raw_trim.to_string(),
        location: location.to_string(),
        reason,
    };

    let (requirement, marker) = match raw_trim.split_once(';') {
        Some((req, marker)) => (req.trim(), Some(marker.trim().to_string())),
        None => (raw_trim, None),
    };

    if let Some(kind) = reference_kind(requirement) {
        return Err(ResolverError::ForbiddenReference {
            raw: raw_trim.to_string(),
            location: location.to_string(),
            kind,
        });
    }

    let name_end = requirement
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'))
        .unwrap_or(requirement.len());
    let raw_name = &requirement[..name_end];
    if !is_valid_python_name(raw_name) {
        return Err(invalid(format!(
            "`{raw_name}` is not a valid Python distribution name"
        )));
    }
    let name = normalize_python_name(raw_name);

    let mut rest = requirement[name_end..].trim();
    let mut extras = Vec::new();
    if let Some(after_bracket) = rest.strip_prefix('[') {
        let close = after_bracket
            .find(']')
            .ok_or_else(|| invalid("unterminated extras `[...]`".to_string()))?;
        for part in after_bracket[..close].split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if !part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
            {
                return Err(invalid(format!("invalid extra name `{part}`")));
            }
            extras.push(part.to_string());
        }
        extras.sort();
        extras.dedup();
        rest = after_bracket[close + 1..].trim();
    }

    let spec = if rest.is_empty() {
        Spec::Other {
            text: String::new(),
            reason: NotMajorOnlyReason::NoSpecifier,
        }
    } else if let Some(after_at) = rest.strip_prefix('@') {
        let after_at = after_at.trim();
        let after_at = after_at
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(after_at);
        if let Some(kind) = reference_kind(after_at) {
            return Err(ResolverError::ForbiddenReference {
                raw: raw_trim.to_string(),
                location: location.to_string(),
                kind,
            });
        }
        match after_at.parse::<u64>() {
            Ok(major) => Spec::Major(major),
            Err(_) => {
                return Err(invalid(format!(
                    "`@{after_at}` is not a major; the jumbo form is `name[extras]@MAJOR`, \
                     e.g. `juntai-fuse-api[http]@2`"
                )))
            }
        }
    } else {
        parse_python_specifier(rest).map_err(invalid)?
    };

    Ok(Declaration {
        raw: raw_trim.to_string(),
        name,
        extras,
        marker,
        spec,
        location: location.to_string(),
    })
}

/// Parse one npm declaration: the package name with its version-range value.
pub fn parse_npm(name: &str, raw: &str, location: &str) -> Result<Declaration, ResolverError> {
    let raw_trim = raw.trim();
    // An unscoped dependency name containing `/` is an npm hosted-Git
    // shorthand (`user/repo`), not a registry package.
    if !name.starts_with('@') && name.contains('/') {
        return Err(ResolverError::ForbiddenReference {
            raw: name.to_string(),
            location: location.to_string(),
            kind: "a hosted-Git shorthand dependency name (`user/repo`)",
        });
    }
    if !is_valid_npm_name(name) {
        return Err(ResolverError::InvalidDeclaration {
            raw: raw_trim.to_string(),
            location: location.to_string(),
            reason: format!("`{name}` is not a valid npm package name"),
        });
    }
    if let Some(kind) = npm_reference_kind(raw_trim) {
        return Err(ResolverError::ForbiddenReference {
            raw: raw_trim.to_string(),
            location: location.to_string(),
            kind,
        });
    }
    Ok(Declaration {
        raw: raw_trim.to_string(),
        name: name.to_string(),
        extras: Vec::new(),
        marker: None,
        spec: classify_npm_range(raw_trim),
        location: location.to_string(),
    })
}

/// Whether an npm name is in an internal scope (`@juntai/*`, legacy `@zephytiju/*`).
pub fn is_npm_internal_name(name: &str) -> bool {
    name.starts_with("@juntai/") || name.starts_with("@zephytiju/")
}

/// PEP 508 name shape: starts and ends with an alphanumeric character, with
/// alphanumerics, `.`, `_`, `-` in between.
fn is_valid_python_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(&first), Some(&last)) => {
            first.is_ascii_alphanumeric()
                && last.is_ascii_alphanumeric()
                && bytes
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        }
        _ => false,
    }
}

fn is_valid_npm_name(name: &str) -> bool {
    let valid_segment = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._~".contains(c))
    };
    if let Some(rest) = name.strip_prefix('@') {
        match rest.split_once('/') {
            Some((scope, pkg)) => valid_segment(scope) && valid_segment(pkg),
            None => false,
        }
    } else {
        valid_segment(name)
    }
}

/// Classify a URL-ish string as a forbidden reference, if it is one.
fn reference_kind(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("git+") || lower.starts_with("git://") || lower.starts_with("ssh://") {
        return Some("a Git URL reference");
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some("a direct artifact URL reference (wheel/tarball)");
    }
    if lower.starts_with("file:") || lower.starts_with("link:") || lower.starts_with("workspace:") {
        return Some("a local path or workspace reference");
    }
    if text.starts_with("./")
        || text.starts_with("../")
        || text.starts_with('/')
        || text == "."
        || text == ".."
    {
        return Some("a local path reference");
    }
    None
}

/// Classify an npm range value as a forbidden reference, if it is one.
fn npm_reference_kind(value: &str) -> Option<&'static str> {
    if let Some(kind) = reference_kind(value) {
        return match kind {
            "a direct artifact URL reference (wheel/tarball)" => {
                Some("a direct tarball URL reference")
            }
            other => Some(other),
        };
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("github:") {
        return Some("a GitHub shorthand reference (`github:user/repo`)");
    }
    None
}

// ---------------------------------------------------------------------------
// Python specifier classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PyOp {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
    Ne,
    Tilde,
    Arbitrary,
}

fn parse_python_clause(text: &str) -> Option<(PyOp, String)> {
    // Longer operators must be tried before their prefixes.
    const OPS: [(&str, PyOp); 8] = [
        ("===", PyOp::Arbitrary),
        ("==", PyOp::Eq),
        ("!=", PyOp::Ne),
        (">=", PyOp::Ge),
        ("<=", PyOp::Le),
        ("~=", PyOp::Tilde),
        (">", PyOp::Gt),
        ("<", PyOp::Lt),
    ];
    let (op, version) = OPS
        .into_iter()
        .find_map(|(prefix, op)| text.strip_prefix(prefix).map(|v| (op, v)))?;
    let version = version.trim();
    if version.is_empty() {
        None
    } else {
        Some((op, version.to_string()))
    }
}

fn parse_python_specifier(text: &str) -> Result<Spec, String> {
    let mut clauses = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("empty clause in specifier `{text}`"));
        }
        match parse_python_clause(part) {
            Some(clause) => clauses.push(clause),
            None => return Err(format!("`{part}` is not a valid version specifier clause")),
        }
    }
    Ok(classify_python_clauses(&clauses, text))
}

/// First dot-separated component parses as an unsigned integer.
fn leading_major(version: &str) -> Option<u64> {
    version.split('.').next()?.parse().ok()
}

/// The whole string parses as an unsigned integer (single component).
fn single_major(version: &str) -> Option<u64> {
    version.parse().ok()
}

/// `M`, `M.0`, `M.0.0`, ... — a floor at exactly the major's zero baseline.
fn zero_floored(version: &str) -> bool {
    let mut components = version.split('.');
    match components.next() {
        Some(first) if first.parse::<u64>().is_ok() => components.all(|c| c == "0"),
        _ => false,
    }
}

/// An exclusive upper bound `<N`, `<N.0`, `<N.0.0` → the major N.
fn upper_bound_major(version: &str) -> Option<u64> {
    if zero_floored(version) {
        leading_major(version)
    } else {
        None
    }
}

fn classify_python_clauses(clauses: &[(PyOp, String)], text: &str) -> Spec {
    let other = |reason: NotMajorOnlyReason| Spec::Other {
        text: text.to_string(),
        reason,
    };
    let unsupported = || {
        other(NotMajorOnlyReason::Unsupported {
            text: text.to_string(),
        })
    };

    if clauses.len() == 1 {
        return match &clauses[0] {
            (PyOp::Eq, version) => {
                if let Some(prefix) = version.strip_suffix(".*") {
                    if let Some(major) = single_major(prefix) {
                        return Spec::Range {
                            major,
                            text: text.to_string(),
                        };
                    }
                }
                other(NotMajorOnlyReason::OverConstrained {
                    text: format!("=={version}"),
                })
            }
            (PyOp::Tilde, version) => {
                let components: Vec<&str> = version.split('.').collect();
                match components.len() {
                    2 if components[1] == "0" => match single_major(components[0]) {
                        Some(major) => Spec::Range {
                            major,
                            text: text.to_string(),
                        },
                        None => unsupported(),
                    },
                    1 => unsupported(), // `~=2` is invalid PEP 440
                    _ => other(NotMajorOnlyReason::OverConstrained {
                        text: format!("~={version}"),
                    }),
                }
            }
            (PyOp::Ge, _) | (PyOp::Gt, _) => other(NotMajorOnlyReason::UnboundedUpper),
            (PyOp::Le, _) | (PyOp::Lt, _) => other(NotMajorOnlyReason::UnboundedLower),
            (PyOp::Ne, _) => unsupported(),
            (PyOp::Arbitrary, version) => other(NotMajorOnlyReason::OverConstrained {
                text: format!("==={version}"),
            }),
        };
    }

    if clauses.len() == 2 {
        let ge = clauses
            .iter()
            .find(|(op, _)| matches!(op, PyOp::Ge))
            .map(|(_, v)| v);
        let lt = clauses
            .iter()
            .find(|(op, _)| matches!(op, PyOp::Lt))
            .map(|(_, v)| v);
        if let (Some(lower), Some(upper)) = (ge, lt) {
            if let (Some(floor), Some(ceil)) = (leading_major(lower), upper_bound_major(upper)) {
                return if ceil == floor + 1 {
                    if zero_floored(lower) {
                        Spec::Range {
                            major: floor,
                            text: text.to_string(),
                        }
                    } else {
                        other(NotMajorOnlyReason::OverConstrained {
                            text: text.to_string(),
                        })
                    }
                } else if ceil > floor + 1 {
                    other(NotMajorOnlyReason::SpansMajors {
                        from: floor,
                        to: ceil,
                    })
                } else {
                    unsupported()
                };
            }
        }
        return unsupported();
    }

    unsupported()
}

// ---------------------------------------------------------------------------
// npm range classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComparatorOp {
    Ge,
    Gt,
    Le,
    Lt,
}

fn parse_comparator(text: &str) -> Option<(ComparatorOp, String)> {
    // Longer operators must be tried before their prefixes.
    const OPS: [(&str, ComparatorOp); 4] = [
        (">=", ComparatorOp::Ge),
        ("<=", ComparatorOp::Le),
        (">", ComparatorOp::Gt),
        ("<", ComparatorOp::Lt),
    ];
    let (op, version) = OPS
        .into_iter()
        .find_map(|(prefix, op)| text.strip_prefix(prefix).map(|v| (op, v)))?;
    let version = version.trim();
    if version.is_empty() {
        None
    } else {
        Some((op, version.to_string()))
    }
}

fn classify_npm_range(raw: &str) -> Spec {
    let other = |reason: NotMajorOnlyReason| Spec::Other {
        text: raw.to_string(),
        reason,
    };
    if raw.is_empty() {
        return other(NotMajorOnlyReason::NoSpecifier);
    }
    let lower = raw.to_ascii_lowercase();
    if matches!(lower.as_str(), "*" | "x" | "latest" | "next") {
        return other(NotMajorOnlyReason::Unsupported {
            text: raw.to_string(),
        });
    }
    if raw.contains(" - ") {
        return other(NotMajorOnlyReason::Unsupported {
            text: raw.to_string(),
        });
    }
    let trimmed = raw.strip_prefix('=').unwrap_or(raw);
    let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed);

    if trimmed.starts_with('^') || trimmed.starts_with('~') {
        let rest = &trimmed[1..];
        return match npm_wildcard_major(rest) {
            Some(major) => Spec::Range {
                major,
                text: raw.to_string(),
            },
            None => other(NotMajorOnlyReason::OverConstrained {
                text: raw.to_string(),
            }),
        };
    }

    let has_separators = raw.contains(',') || raw.chars().any(|c| c.is_ascii_whitespace());
    if !has_separators {
        // A single comparator token (`>=2`, `<3`) is unbounded in one
        // direction; otherwise it is a version token: `1`, `1.x`, `1.2`, ...
        if let Some((op, _)) = parse_comparator(trimmed) {
            return match op {
                ComparatorOp::Ge | ComparatorOp::Gt => other(NotMajorOnlyReason::UnboundedUpper),
                ComparatorOp::Le | ComparatorOp::Lt => other(NotMajorOnlyReason::UnboundedLower),
            };
        }
        return npm_single_token(trimmed, raw);
    }

    // Comparator list separated by commas and/or whitespace.
    let parts: Vec<&str> = trimmed
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    match parts.len() {
        1 => match parse_comparator(parts[0]).map(|(op, _)| op) {
            Some(ComparatorOp::Ge) | Some(ComparatorOp::Gt) => {
                other(NotMajorOnlyReason::UnboundedUpper)
            }
            Some(ComparatorOp::Le) | Some(ComparatorOp::Lt) => {
                other(NotMajorOnlyReason::UnboundedLower)
            }
            None => other(NotMajorOnlyReason::Unsupported {
                text: raw.to_string(),
            }),
        },
        2 => {
            let lower = parts
                .iter()
                .filter_map(|p| parse_comparator(p))
                .find(|(op, _)| matches!(op, ComparatorOp::Ge))
                .map(|(_, v)| v);
            let upper = parts
                .iter()
                .filter_map(|p| parse_comparator(p))
                .find(|(op, _)| matches!(op, ComparatorOp::Lt))
                .map(|(_, v)| v);
            if let (Some(lower), Some(upper)) = (lower, upper) {
                if let (Some(floor), Some(ceil)) =
                    (leading_major(&lower), upper_bound_major(&upper))
                {
                    return if ceil == floor + 1 {
                        if zero_floored(&lower) {
                            Spec::Range {
                                major: floor,
                                text: raw.to_string(),
                            }
                        } else {
                            other(NotMajorOnlyReason::OverConstrained {
                                text: raw.to_string(),
                            })
                        }
                    } else if ceil > floor + 1 {
                        other(NotMajorOnlyReason::SpansMajors {
                            from: floor,
                            to: ceil,
                        })
                    } else {
                        other(NotMajorOnlyReason::Unsupported {
                            text: raw.to_string(),
                        })
                    };
                }
            }
            other(NotMajorOnlyReason::Unsupported {
                text: raw.to_string(),
            })
        }
        _ => other(NotMajorOnlyReason::Unsupported {
            text: raw.to_string(),
        }),
    }
}

/// `M`, `M.x`, `M.X.*`, ... — a bare major with only wildcard components.
fn npm_wildcard_major(text: &str) -> Option<u64> {
    let mut components = text.split('.');
    let major = components.next()?.parse().ok()?;
    if components.all(|c| matches!(c, "x" | "X" | "*")) {
        Some(major)
    } else {
        None
    }
}

fn npm_single_token(token: &str, raw: &str) -> Spec {
    let other = |reason: NotMajorOnlyReason| Spec::Other {
        text: raw.to_string(),
        reason,
    };
    if token.is_empty() {
        return other(NotMajorOnlyReason::NoSpecifier);
    }
    let mut components = token.split('.');
    match components.next() {
        Some(first) if first.parse::<u64>().is_ok() => {
            let rest: Vec<&str> = components.collect();
            if rest.is_empty() || rest.iter().all(|c| matches!(*c, "x" | "X" | "*")) {
                let major = first.parse().unwrap();
                Spec::Range {
                    major,
                    text: raw.to_string(),
                }
            } else {
                // `1.2` / `1.2.3` / `1.2.x` constrain inside the major.
                other(NotMajorOnlyReason::OverConstrained {
                    text: raw.to_string(),
                })
            }
        }
        _ => other(NotMajorOnlyReason::Unsupported {
            text: raw.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOC: &str = "test";

    fn python(raw: &str) -> Result<Declaration, ResolverError> {
        parse_python(raw, LOC)
    }

    #[test]
    fn python_jumbo_major_form_is_accepted() {
        let d = python("juntai-fuse-api[http]@2").expect("parse");
        assert_eq!(d.name, "juntai-fuse-api");
        assert_eq!(d.extras, vec!["http"]);
        assert_eq!(d.spec, Spec::Major(2));

        let d = python("demo @ 3").expect("parse with spaces");
        assert_eq!(d.spec, Spec::Major(3));

        let d = python("demo_alpha[http,async,b]@10 ; python_version >= '3.11'").expect("parse");
        assert_eq!(d.name, "demo-alpha"); // PEP 503 normalization
        assert_eq!(d.extras, vec!["async", "b", "http"]);
        assert_eq!(d.marker.as_deref(), Some("python_version >= '3.11'"));
        assert_eq!(d.spec, Spec::Major(10));
    }

    #[test]
    fn python_collapsing_ranges_are_accepted() {
        for (raw, major) in [
            ("demo==2.*", 2),
            ("demo~=2.0", 2),
            ("demo>=2,<3", 2),
            ("demo>=2.0,<3", 2),
        ] {
            let d = python(raw).expect("parse");
            assert!(
                matches!(d.spec, Spec::Range { major: m, .. } if m == major),
                "`{raw}` should collapse to major {major}, got {:?}",
                d.spec
            );
            assert_eq!(d.spec.major(), Some(major));
        }
    }

    #[test]
    fn python_multi_major_and_unbounded_ranges_are_rejected_for_internal() {
        let d = python("demo>=1,<3").expect("parse");
        assert_eq!(
            d.spec.not_major_only_reason(),
            NotMajorOnlyReason::SpansMajors { from: 1, to: 3 }
        );
        let d = python("demo>=2").expect("parse");
        assert_eq!(
            d.spec.not_major_only_reason(),
            NotMajorOnlyReason::UnboundedUpper
        );
        let d = python("demo<3").expect("parse");
        assert_eq!(
            d.spec.not_major_only_reason(),
            NotMajorOnlyReason::UnboundedLower
        );
        let d = python("demo").expect("parse");
        assert_eq!(
            d.spec.not_major_only_reason(),
            NotMajorOnlyReason::NoSpecifier
        );
    }

    #[test]
    fn python_over_constrained_forms_are_rejected_for_internal() {
        for raw in [
            "demo==2.1.3",
            "demo==2",
            "demo===2.1.3",
            "demo~=2.1",
            "demo~=2.0.1",
            "demo>=2.2,<3",
            "demo==2.1.*",
        ] {
            let d = python(raw).expect("parse");
            assert!(
                matches!(
                    d.spec.not_major_only_reason(),
                    NotMajorOnlyReason::OverConstrained { .. }
                ),
                "`{raw}` should be over-constrained, got {:?}",
                d.spec
            );
        }
        // `!=` and contradictory sets are unsupported, not major-only.
        for raw in ["demo!=2.5.0", "demo>=3,<2", "demo>2,<3"] {
            let d = python(raw).expect("parse");
            assert!(
                matches!(
                    d.spec.not_major_only_reason(),
                    NotMajorOnlyReason::Unsupported { .. }
                ),
                "`{raw}` should be unsupported, got {:?}",
                d.spec
            );
        }
    }

    #[test]
    fn python_git_and_artifact_urls_are_forbidden() {
        for raw in [
            "demo @ git+https://github.com/org/repo.git",
            "demo @ https://github.com/org/repo/releases/download/v1/demo-1.0.0-py3-none-any.whl",
            "demo @ https://files.example.org/demo-1.0.0.tar.gz",
            "git+https://github.com/org/repo.git#egg=demo",
            "https://files.example.org/demo-1.0.0.whl",
            "demo @ file:///tmp/demo",
            "demo @ ../local/demo",
            "./local/demo",
        ] {
            let err = python(raw).unwrap_err();
            assert!(
                matches!(err, ResolverError::ForbiddenReference { .. }),
                "`{raw}` should be a forbidden reference, got: {err}"
            );
        }
    }

    #[test]
    fn python_invalid_declarations_error() {
        assert!(python("demo@2.1").is_err()); // not an integer major
        assert!(python("demo@v2").is_err());
        assert!(python("demo[http@2").is_err()); // unterminated extras
        assert!(python("").is_err());
        assert!(python("demo>=2,<").is_err()); // missing version
        assert!(python("demo^1").is_err()); // not a Python specifier
    }

    #[test]
    fn python_third_party_ranges_pass_through() {
        let d = python("numpy>=1.26").expect("parse");
        assert_eq!(d.name, "numpy");
        assert!(d.spec.major().is_none());
        let d = python("requests").expect("parse");
        assert_eq!(
            d.spec.not_major_only_reason(),
            NotMajorOnlyReason::NoSpecifier
        );
    }

    #[test]
    fn npm_major_only_forms_are_accepted() {
        for (value, major) in [
            ("1", 1),
            ("1.x", 1),
            ("1.X", 1),
            ("1.*", 1),
            ("1.x.x", 1),
            ("^2", 2),
            ("~2", 2),
            ("=3", 3),
            (">=2,<3", 2),
            (">=2 <3", 2),
            (">=2.0,<3", 2),
        ] {
            let d = parse_npm("@juntai/kit", value, LOC).expect("parse");
            assert!(
                matches!(d.spec, Spec::Range { major: m, .. } if m == major),
                "value `{value}` should collapse to major {major}, got {:?}",
                d.spec
            );
        }
    }

    #[test]
    fn npm_non_major_forms_are_classified_with_reasons() {
        let d = parse_npm("@juntai/kit", ">=1,<3", LOC).expect("parse");
        assert_eq!(
            d.spec.not_major_only_reason(),
            NotMajorOnlyReason::SpansMajors { from: 1, to: 3 }
        );
        for (value, expected) in [
            ("", NotMajorOnlyReason::NoSpecifier),
            ("*", NotMajorOnlyReason::Unsupported { text: "*".into() }),
            (
                "latest",
                NotMajorOnlyReason::Unsupported {
                    text: "latest".into(),
                },
            ),
            (">=1", NotMajorOnlyReason::UnboundedUpper),
            ("<2", NotMajorOnlyReason::UnboundedLower),
            (
                "1.2.3",
                NotMajorOnlyReason::OverConstrained {
                    text: "1.2.3".into(),
                },
            ),
            (
                "1.2",
                NotMajorOnlyReason::OverConstrained { text: "1.2".into() },
            ),
            (
                "^1.2",
                NotMajorOnlyReason::OverConstrained {
                    text: "^1.2".into(),
                },
            ),
            (
                "~1.2",
                NotMajorOnlyReason::OverConstrained {
                    text: "~1.2".into(),
                },
            ),
            (
                ">=1.2,<2",
                NotMajorOnlyReason::OverConstrained {
                    text: ">=1.2,<2".into(),
                },
            ),
        ] {
            let d = parse_npm("@juntai/kit", value, LOC).expect("parse");
            assert_eq!(d.spec.not_major_only_reason(), expected, "value `{value}`");
        }
    }

    #[test]
    fn npm_git_and_url_references_are_forbidden() {
        for value in [
            "git+https://github.com/org/repo.git",
            "git://github.com/org/repo.git",
            "ssh://git@github.com/org/repo.git",
            "github:org/repo",
            "https://github.com/org/repo/-/kit-1.0.0.tgz",
            "https://registry.example.org/kit/-/kit-1.0.0.tgz",
            "file:../kit",
            "link:../kit",
            "../kit",
        ] {
            let err = parse_npm("@juntai/kit", value, LOC).unwrap_err();
            assert!(
                matches!(err, ResolverError::ForbiddenReference { .. }),
                "value `{value}` should be forbidden, got: {err}"
            );
        }
        // An unscoped dependency name containing `/` is a hosted-Git shorthand.
        for name in ["org/repo", "org/repo#semver:^1"] {
            let err = parse_npm(name, "*", LOC).unwrap_err();
            assert!(
                matches!(err, ResolverError::ForbiddenReference { .. }),
                "name `{name}` should be forbidden, got: {err}"
            );
        }
        // Unscoped third-party names with tarball values are rejected too.
        let err = parse_npm("lodash", "https://example.com/lodash.tgz", LOC).unwrap_err();
        assert!(matches!(err, ResolverError::ForbiddenReference { .. }));
    }

    #[test]
    fn npm_third_party_ranges_pass_through() {
        for value in ["^4.17.21", ">=1.2 <2", "*", "2.0.0", "~2.1.0", ">=2 <3"] {
            let d = parse_npm("lodash", value, LOC).expect("parse");
            assert_eq!(d.name, "lodash");
            // Pass-through classification must not error.
            let _ = d.spec;
        }
    }

    #[test]
    fn npm_internal_scope_detection() {
        assert!(is_npm_internal_name("@juntai/kit"));
        assert!(is_npm_internal_name("@zephytiju/vangu-constructs"));
        assert!(!is_npm_internal_name("@types/node"));
        assert!(!is_npm_internal_name("lodash"));
    }
}
