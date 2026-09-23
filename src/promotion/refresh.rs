//! The third-party refresh policy knob (Jumbo Build & Versioning Standard,
//! §2.3, "refresh policy").
//!
//! Jumbo generates the lock by resolving the manifest — internal
//! dependencies from the index, third-party dependencies from their
//! declared ranges — and by default every pipeline run re-resolves those
//! declared ranges (`uv lock --upgrade`; npm's lock-only install). When
//! build-minute cost from third-party freshness waves needs bounding, the
//! knob switches re-resolution to a cadence:
//!
//! - `run` (default): re-resolve every run. A new in-range third-party
//!   version changes the canonical extract, which promotes a patch bump —
//!   the mechanism that keeps builds fresh by default.
//! - `schedule:<interval>` (e.g. `24h`, `30m`, `7d`, `2w`): re-resolve only
//!   when the cadence has elapsed since the package's newest index record
//!   of the declared major (by its recorded timestamp; no record at all —
//!   e.g. the bootstrap build — counts as due). Between cadence points the
//!   gate suppresses the `--upgrade` step, so the build reuses the current
//!   resolution: the extract is unchanged and no patch churn is produced.
//! - `schedule:<cron>` (five space-separated fields): recorded verbatim and
//!   evaluated as due. The cadence itself is honored by the executor's
//!   pipeline schedule (the workflow that invokes jumbo on that cron), not
//!   inside a single run — "when the last cron tick happened" is not
//!   derivable from the index, so jumbo fails safe toward re-resolving.
//!
//! The evaluated gate is recorded in the promotion decision output, and it
//! is the single switch for the lock tool invocation (`jumbo lock
//! --refresh=…` and `jumbo promote`'s own lock generation): `upgrade=false`
//! runs `uv lock` without `--upgrade` (npm has no separate flag — its
//! lock-only install already prefers the existing pins within the declared
//! ranges whenever a lock exists).
//!
//! Configuration precedence: the `--refresh` CLI flag, then the
//! `JUMBO_REFRESH` environment variable, then the default `run`.

use serde::Serialize;

/// Environment variable holding the default refresh policy.
pub const REFRESH_ENV: &str = "JUMBO_REFRESH";

/// A parsed refresh policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshPolicy {
    /// Re-resolve third-party ranges on every run (the default).
    Run,
    /// Re-resolve on a cadence.
    Schedule {
        /// The schedule verbatim (e.g. `7d` or a cron expression).
        schedule: String,
        /// How the schedule is evaluated.
        kind: ScheduleKind,
    },
}

/// How a scheduled refresh policy is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleKind {
    /// `<N><unit>` with `m`/`h`/`d`/`w`; jumbo evaluates due/overdue itself.
    Interval { seconds: u64 },
    /// A five-field cron expression; the executor's schedule honors the
    /// cadence, jumbo re-resolves when invoked (fail-safe).
    Cron,
}

impl RefreshPolicy {
    /// Parse a `--refresh` / `JUMBO_REFRESH` value.
    pub fn parse(spec: &str) -> Result<Self, RefreshError> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err(RefreshError {
                spec: spec.to_string(),
                reason: "empty refresh policy".into(),
            });
        }
        if trimmed == "run" {
            return Ok(Self::Run);
        }
        let Some(schedule) = trimmed.strip_prefix("schedule:") else {
            return Err(RefreshError {
                spec: spec.to_string(),
                reason: format!(
                    "unknown refresh policy `{trimmed}`; accepted: run, schedule:<interval> \
                     (e.g. 24h, 30m, 7d, 2w), schedule:<cron> (5 fields)"
                ),
            });
        };
        if schedule.is_empty() {
            return Err(RefreshError {
                spec: spec.to_string(),
                reason: "schedule: needs an interval (e.g. 24h, 7d) or a 5-field cron".into(),
            });
        }
        if let Some(seconds) = parse_interval(schedule) {
            return Ok(Self::Schedule {
                schedule: schedule.to_string(),
                kind: ScheduleKind::Interval { seconds },
            });
        }
        if schedule.split_whitespace().count() == 5 {
            return Ok(Self::Schedule {
                schedule: schedule.to_string(),
                kind: ScheduleKind::Cron,
            });
        }
        Err(RefreshError {
            spec: spec.to_string(),
            reason: format!(
                "`{schedule}` is neither an interval (`<N>` + m/h/d/w, e.g. 24h, 7d) nor a \
                 5-field cron expression"
            ),
        })
    }

    /// The canonical knob spelling, for recording in outputs.
    pub fn as_spec(&self) -> String {
        match self {
            Self::Run => "run".to_string(),
            Self::Schedule { schedule, .. } => format!("schedule:{schedule}"),
        }
    }
}

/// `<N><unit>` with unit `m`/`h`/`d`/`w` → seconds; `None` when malformed.
fn parse_interval(text: &str) -> Option<u64> {
    let (digits, unit) = text.split_at(text.len().checked_sub(1)?);
    let count: u64 = digits.parse().ok()?;
    if count == 0 {
        return None;
    }
    let per = match unit {
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    Some(count.saturating_mul(per))
}

/// The evaluated refresh gate for one run.
///
/// `upgrade` is the single switch the lock-tool invocation consumes;
/// `due` is the raw cadence answer (always true in `run` mode and for cron
/// schedules, which the executor's schedule honors instead).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshGate {
    /// The knob value the gate was evaluated from (verbatim spelling).
    pub policy: String,
    /// `run` or `schedule`.
    pub mode: &'static str,
    /// The schedule (interval or cron text); null in `run` mode.
    pub schedule: Option<String>,
    /// `interval` or `cron`; null in `run` mode.
    pub schedule_kind: Option<&'static str>,
    /// Whether third-party re-resolution is due this run.
    pub due: bool,
    /// Whether the lock tool's `--upgrade` step runs this run. When false,
    /// the build reuses the current resolution — extract unchanged, no
    /// patch churn.
    pub upgrade: bool,
    /// How the cadence was decided (recorded for auditability).
    pub basis: &'static str,
}

/// Evaluate the gate for one run.
///
/// `newest_record_timestamp` is the timestamp of the package's newest
/// index record of the declared major (the last promoted build — the only
/// refresh-relevant time observable in the index; runs that re-resolved
/// without producing a record are invisible, so a cadence errs toward
/// re-resolving). `None` when no record exists.
pub fn evaluate(
    policy: &RefreshPolicy,
    newest_record_timestamp: Option<&str>,
    now_unix: u64,
) -> RefreshGate {
    match policy {
        RefreshPolicy::Run => RefreshGate {
            policy: policy.as_spec(),
            mode: "run",
            schedule: None,
            schedule_kind: None,
            due: true,
            upgrade: true,
            basis: "default policy: third-party ranges re-resolve every run",
        },
        RefreshPolicy::Schedule {
            schedule,
            kind: ScheduleKind::Interval { seconds },
        } => {
            let (due, basis) = match newest_record_timestamp.and_then(parse_rfc3339_unix) {
                None => (
                    true,
                    "no readable timestamp on the newest index record: re-resolution due \
                     (fail-safe)",
                ),
                Some(recorded) => {
                    if now_unix >= recorded.saturating_add(*seconds) {
                        (
                            true,
                            "cadence elapsed since the newest index record's timestamp",
                        )
                    } else {
                        (
                            false,
                            "cadence not elapsed since the newest index record's timestamp",
                        )
                    }
                }
            };
            RefreshGate {
                policy: policy.as_spec(),
                mode: "schedule",
                schedule: Some(schedule.clone()),
                schedule_kind: Some("interval"),
                due,
                upgrade: due,
                basis,
            }
        }
        RefreshPolicy::Schedule {
            schedule,
            kind: ScheduleKind::Cron,
        } => RefreshGate {
            policy: policy.as_spec(),
            mode: "schedule",
            schedule: Some(schedule.clone()),
            schedule_kind: Some("cron"),
            due: true,
            upgrade: true,
            basis: "cron cadences are honored by the executor's pipeline schedule; jumbo \
                    re-resolves when invoked (fail-safe)",
        },
    }
}

/// Resolve the effective policy: the CLI flag, then `JUMBO_REFRESH`, then
/// the default `run`.
pub fn resolve_policy(flag: Option<&str>) -> Result<RefreshPolicy, RefreshError> {
    if let Some(spec) = flag {
        return RefreshPolicy::parse(spec);
    }
    if let Ok(spec) = std::env::var(REFRESH_ENV) {
        if !spec.trim().is_empty() {
            return RefreshPolicy::parse(&spec);
        }
    }
    Ok(RefreshPolicy::Run)
}

/// A refresh policy that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid refresh policy `{spec}`: {reason}")]
pub struct RefreshError {
    pub spec: String,
    pub reason: String,
}

/// Parse an RFC 3339 timestamp (`2026-09-01T00:00:00Z`, optional
/// fractional seconds, `Z` or `±HH:MM` offset) to Unix seconds.
fn parse_rfc3339_unix(s: &str) -> Option<u64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u64> {
        let part = s.get(range)?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let year = digits(0..4)? as i64;
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let month = digits(5..7)?;
    let day = digits(8..10)?;
    if bytes[10] != b'T' && bytes[10] != b't' && bytes[10] != b' ' {
        return None;
    }
    let hour = digits(11..13)?;
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let minute = digits(14..16)?;
    let second = digits(17..19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Skip optional fractional seconds.
    let mut rest = &s[19..];
    if rest.starts_with('.') {
        let end = rest[1..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    let offset_seconds: i64 = if rest.is_empty() || rest == "Z" || rest == "z" {
        0
    } else {
        let (sign, rest) = match rest.as_bytes()[0] {
            b'+' => (1i64, &rest[1..]),
            b'-' => (-1i64, &rest[1..]),
            _ => return None,
        };
        let mut parts = rest.split(':');
        let oh: i64 = parts.next()?.parse().ok()?;
        let om: i64 = parts.next().unwrap_or("0").parse().ok()?;
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 3600 + om * 60)
    };
    let days = days_from_civil(year, month, day);
    let unix = days * 86_400 + (hour * 3600 + minute * 60 + second) as i64 - offset_seconds;
    u64::try_from(unix).ok()
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u64, d: u64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_run_and_intervals_and_cron() {
        assert_eq!(RefreshPolicy::parse("run"), Ok(RefreshPolicy::Run));
        for (spec, seconds) in [
            ("schedule:30m", 1_800),
            ("schedule:24h", 86_400),
            ("schedule:7d", 604_800),
            ("schedule:2w", 1_209_600),
        ] {
            assert_eq!(
                RefreshPolicy::parse(spec),
                Ok(RefreshPolicy::Schedule {
                    schedule: spec.strip_prefix("schedule:").unwrap().to_string(),
                    kind: ScheduleKind::Interval { seconds }
                }),
                "`{spec}` should parse as an interval"
            );
        }
        // Five-field cron passes through verbatim.
        assert_eq!(
            RefreshPolicy::parse("schedule:0 3 * * *"),
            Ok(RefreshPolicy::Schedule {
                schedule: "0 3 * * *".to_string(),
                kind: ScheduleKind::Cron
            })
        );
        // Whitespace is trimmed.
        assert_eq!(RefreshPolicy::parse("  run  "), Ok(RefreshPolicy::Run));
    }

    #[test]
    fn malformed_policies_are_rejected_with_guidance() {
        for bad in [
            "",
            "every",
            "schedule",
            "schedule:",
            "schedule:soon",
            "schedule:0h",
            "schedule:7days",
            "schedule:6x",
            "schedule:0 3 *",
        ] {
            let err = RefreshPolicy::parse(bad).unwrap_err();
            assert!(
                err.to_string().contains("invalid refresh policy"),
                "`{bad}` should be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn run_mode_always_upgrades() {
        let gate = evaluate(&RefreshPolicy::Run, Some("2020-01-01T00:00:00Z"), 0);
        assert!(gate.due);
        assert!(gate.upgrade);
        assert_eq!(gate.mode, "run");
        assert_eq!(gate.schedule, None);
        assert_eq!(gate.schedule_kind, None);
    }

    #[test]
    fn interval_gate_is_due_when_cadence_elapsed() {
        let policy = RefreshPolicy::parse("schedule:7d").unwrap();
        let recorded = 1_800_000_000u64; // 2027-01-15T08:00:00Z
                                         // Eight days later: due.
        let gate = evaluate(&policy, Some("2027-01-15T08:00:00Z"), recorded + 8 * 86_400);
        assert!(gate.due);
        assert!(gate.upgrade);
        // Three days later: suppressed — reuse the current resolution.
        let gate = evaluate(&policy, Some("2027-01-15T08:00:00Z"), recorded + 3 * 86_400);
        assert!(!gate.due);
        assert!(!gate.upgrade);
        // Exactly at the cadence boundary: due (>=).
        let gate = evaluate(&policy, Some("2027-01-15T08:00:00Z"), recorded + 7 * 86_400);
        assert!(gate.due);
    }

    #[test]
    fn interval_gate_fails_safe_without_a_readable_timestamp() {
        let policy = RefreshPolicy::parse("schedule:24h").unwrap();
        // No record at all (bootstrap build): due.
        let gate = evaluate(&policy, None, 0);
        assert!(gate.due);
        assert!(gate.upgrade);
        // Unparseable timestamp: fail safe toward re-resolving.
        let gate = evaluate(&policy, Some("not-a-timestamp"), 0);
        assert!(gate.due);
        assert!(gate.upgrade);
    }

    #[test]
    fn cron_gate_re_resolves_within_the_run() {
        let policy = RefreshPolicy::parse("schedule:0 3 * * *").unwrap();
        let gate = evaluate(&policy, Some("2027-01-15T08:00:00Z"), 1_800_000_000);
        assert_eq!(gate.schedule_kind, Some("cron"));
        assert!(gate.due);
        assert!(gate.upgrade);
        assert!(gate.basis.contains("executor"));
    }

    #[test]
    fn rfc3339_parsing_matches_known_instants() {
        assert_eq!(parse_rfc3339_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_unix("2026-09-01T00:00:00Z"),
            Some(1_788_220_800)
        );
        // Fractional seconds are skipped; offsets shift the instant.
        assert_eq!(
            parse_rfc3339_unix("2026-09-01T00:00:00.123Z"),
            Some(1_788_220_800)
        );
        assert_eq!(
            parse_rfc3339_unix("2026-09-01T02:00:00+02:00"),
            Some(1_788_220_800)
        );
        assert_eq!(
            parse_rfc3339_unix("2026-08-31T23:00:00-01:00"),
            Some(1_788_220_800)
        );
        // Leap-year day.
        assert_eq!(
            parse_rfc3339_unix("2028-02-29T12:00:00Z"),
            Some(1_835_438_400)
        );
        for bad in [
            "",
            "2026-09-01",
            "2026-13-01T00:00:00Z",
            "2026-09-32T00:00:00Z",
            "2026-09-01T25:00:00Z",
            "2026-09-01T00:00:00+99:00",
            "yesterday",
        ] {
            assert_eq!(parse_rfc3339_unix(bad), None, "`{bad}` should not parse");
        }
    }
}
