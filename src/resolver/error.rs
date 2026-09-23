//! Typed errors for the Jumbo resolver core.
//!
//! Every error message is written to be actionable on its own: it names the
//! offending declaration, where it was found, and — for absorption and
//! manifest-rule violations — the step required to fix it, pointing at the
//! Jumbo Build & Versioning Standard and the JumboIndex repository.

// The resolver error enum carries rich context strings for actionable
// messages and flows through `anyhow` at the CLI boundary, where its
// stack size is not performance-relevant.
#![allow(clippy::result_large_err)]
use std::fmt;

/// Why a dependency declaration does not collapse to exactly one major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotMajorOnlyReason {
    /// No version is declared at all.
    NoSpecifier,
    /// Only an upper bound is present; the major is unbounded below.
    UnboundedLower,
    /// Only a lower bound is present; the range is unbounded above.
    UnboundedUpper,
    /// The range covers several majors, e.g. `>=1,<3`.
    SpansMajors { from: u64, to: u64 },
    /// The declaration pins or floors minor/patch segments, which the
    /// pipeline owns, e.g. `==2.1.3`, `~=2.1`, `^1.2`.
    OverConstrained { text: String },
    /// Any other specifier form that is not one of the accepted major-only
    /// forms (`!=`, hyphen ranges, tags, wildcards, ...).
    Unsupported { text: String },
}

impl fmt::Display for NotMajorOnlyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSpecifier => write!(f, "no version is declared"),
            Self::UnboundedLower => write!(f, "only an upper bound is declared; the major is unbounded below"),
            Self::UnboundedUpper => write!(f, "only a lower bound is declared; the range is unbounded above"),
            Self::SpansMajors { from, to } => {
                write!(f, "spans multiple majors ({from}..{})", to.saturating_sub(1))
            }
            Self::OverConstrained { text } => write!(
                f,
                "`{text}` constrains minor/patch segments, which the pipeline owns; declare the major only"
            ),
            Self::Unsupported { text } => {
                write!(f, "`{text}` is not one of the accepted major-only forms")
            }
        }
    }
}

/// Errors produced by the resolver core.
#[derive(Debug, thiserror::Error)]
pub enum ResolverError {
    #[error("invalid dependency declaration `{raw}` in {location}: {reason}")]
    InvalidDeclaration {
        raw: String,
        location: String,
        reason: String,
    },

    #[error(
        "forbidden dependency reference `{raw}` in {location}: {kind}.\n  \
         Jumbo rule: internal packages are declared by major and resolved from the Jumbo index; \
         a manifest never carries a Git URL or a direct artifact URL \
         (Jumbo Build & Versioning Standard, Resolution Semantics)."
    )]
    ForbiddenReference {
        raw: String,
        location: String,
        kind: &'static str,
    },

    #[error(
        "internal dependency `{name}` is declared as `{raw}` in {location}, which is not major-only: {reason}.\n  \
         Accepted {ecosystem} forms: {accepted}"
    )]
    NotMajorOnly {
        name: String,
        raw: String,
        location: String,
        reason: NotMajorOnlyReason,
        ecosystem: &'static str,
        accepted: &'static str,
    },

    #[error(
        "absorption error: internal dependency `{name}` (declared major {major}) from {location} has no record \
         in the Jumbo index (https://github.com/zephytiju/JumboIndex).\n  \
         Absorption step: bring the package's repository into the Jumbo build system — cover it with a \
         CircleCI umbrella pipeline (private repositories) or call the reusable jumbo-publish GitHub Actions \
         workflow from it (public repositories) so every promoted build appends an index record. \
         The dependency cannot be consumed until its repository is absorbed \
         (Jumbo Build & Versioning Standard, Resolution Semantics)."
    )]
    Absorption {
        name: String,
        major: u64,
        location: String,
    },

    #[error(
        "no major-{major} record for `{name}` in the Jumbo index. Available majors: {available}. \
         Consumers stay on their declared major until they declare a new one."
    )]
    NoRecordForMajor {
        name: String,
        major: u64,
        available: String,
    },

    #[error(
        "package `{name}` has no record in the Jumbo index and is not internal \
         (neither @juntai/@zephytiju npm scope nor jumbo major-only syntax). \
         Third-party dependencies are resolved by the normal language tooling, not by jumbo."
    )]
    NotInternalPackage { name: String },

    #[error("cannot read the Jumbo index from `{origin}`: {reason}")]
    IndexUnavailable { origin: String, reason: String },

    #[error("invalid index record in {file} at line {line}: {reason}")]
    InvalidRecord {
        file: String,
        line: usize,
        reason: String,
    },

    #[error("unsupported manifest `{path}`: {reason}")]
    InvalidManifest { path: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_major_only_reason_renders_specific_messages() {
        assert_eq!(
            NotMajorOnlyReason::SpansMajors { from: 1, to: 3 }.to_string(),
            "spans multiple majors (1..2)"
        );
        assert_eq!(
            NotMajorOnlyReason::NoSpecifier.to_string(),
            "no version is declared"
        );
        assert!(NotMajorOnlyReason::OverConstrained {
            text: "==2.1.3".into()
        }
        .to_string()
        .contains("pipeline owns"));
    }
}
