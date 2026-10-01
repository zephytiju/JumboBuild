//! The validated github.com-only artifact fetch layer
//! (Jumbo Build & Versioning Standard, §3.4 Artifact Storage and
//! Materialization).
//!
//! JumboBuild downloads release assets **by exact URL** and verifies the
//! recorded SHA-256; no registry protocol (pip, npm) is ever contacted for
//! an internal package. The layer is deliberately narrow:
//!
//! - `https` only, and the host must be one of `github.com` (where release
//!   asset URLs live), GitHub's release-asset CDN hosts
//!   (`objects.githubusercontent.com`, `release-assets.githubusercontent.com`,
//!   to where a download redirects), or `codeload.github.com` (GitHub's
//!   tarball host, from where the source fallback fetches repository trees
//!   at a recorded commit). Everything else — other hosts, localhost,
//!   loopback/private/reserved addresses, IP literals, userinfo, and
//!   non-default ports — is rejected before any bytes move. The one
//!   exception is the private-release fallback route below, which talks to
//!   `api.github.com` and to nothing else.
//! - Redirects are followed manually, one hop at a time through this same
//!   validation, so a redirect can never widen the egress surface.
//! - Credentials come only from the environment (`GITHUB_TOKEN`,
//!   `GH_TOKEN`) or from `gh auth token`. The token is handed to curl via
//!   a mode-0600 config file, never on the command line (where `ps` could
//!   read it), never in a log line, and never in an error message.
//!
//! # The private-release fallback route (api.github.com)
//!
//! On a PRIVATE repository the recorded
//! `github.com/<o>/<r>/releases/download/<tag>/<asset>` URL answers **404
//! even with an Authorization header**: GitHub serves private release
//! assets only through the authenticated REST asset route. When the first
//! hop (the recorded URL itself) answers 404 and a token is present, the
//! layer therefore resolves the asset on that route: `GET
//! api.github.com/repos/<o>/<r>/releases/tags/<tag>` finds the release,
//! the asset is matched by its exact file name, and `GET
//! .../releases/assets/<id>` with `Accept: application/octet-stream`
//! returns the bytes through the same release-asset CDN redirect a public
//! download takes. `api.github.com` is allowed on THIS route only — a
//! recorded artifact URL pointing there is still rejected by the general
//! artifact validation — and every URL on the route keeps the strict
//! https / no-userinfo / default-port / no-IP-literal rules. The public
//! path is untouched: a public asset 302s from the recorded URL and never
//! enters the fallback.
//!
//! The host rules are pure functions so they are fully testable offline;
//! `download_artifact` itself is a thin curl driver over them.

use std::path::{Path, PathBuf};

use super::error::MaterializeError;

/// Hosts an artifact URL (and every redirect hop) may point at.
pub const ALLOWED_ARTIFACT_HOSTS: [&str; 4] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    // GitHub's tarball host: the source fallback fetches repository trees
    // at a recorded commit from here.
    "codeload.github.com",
];

/// Environment variables a GitHub token is accepted from, in order.
pub const TOKEN_ENV_VARS: [&str; 2] = ["GITHUB_TOKEN", "GH_TOKEN"];

/// The GitHub REST API host the private-release fallback route calls.
/// Allowed on THAT ROUTE ONLY: a recorded artifact URL pointing here is
/// still rejected by the general artifact validation — release assets are
/// downloaded from `github.com` and the release-asset CDN hosts, while the
/// API host serves the by-tag/by-id asset resolution a private repository
/// needs ([`fetch_release_asset_via_api`]).
pub const API_HOST: &str = "api.github.com";

/// Maximum redirect hops before the fetch gives up.
const MAX_REDIRECTS: usize = 5;

/// A validated artifact URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactUrl {
    /// The exact URL as recorded in the index record.
    pub url: String,
    /// Host it points at (already lowercase).
    pub host: String,
    /// Final path segment: the asset's file name (`pkg-2.4.0-py3-none-any.whl`).
    pub file_name: String,
}

impl ArtifactUrl {
    /// Re-validate a URL of a redirect hop.
    fn hop(raw: &str) -> Result<Self, MaterializeError> {
        validate_artifact_url(raw)
    }
}

/// Whether a host is an IP literal (v4 or v6, bracketed or not).
fn is_ip_literal(host: &str) -> bool {
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    stripped.parse::<std::net::IpAddr>().is_ok()
}

/// Whether an IP literal falls in loopback, private, or reserved ranges —
/// reported distinctly so SSRF-shaped URLs fail with a precise reason.
fn ip_literal_reason(host: &str) -> Option<&'static str> {
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    let addr: std::net::IpAddr = stripped.parse().ok()?;
    if addr.is_loopback() {
        Some("loopback address")
    } else if addr.is_unspecified() {
        Some("unspecified address")
    } else {
        match addr {
            std::net::IpAddr::V4(v4) => {
                if v4.is_private()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_documentation()
                    || v4.is_unspecified()
                {
                    Some("private or reserved address")
                } else {
                    Some("IP-literal host")
                }
            }
            std::net::IpAddr::V6(v6) => {
                if v6.segments()[0] & 0xffc0 == 0xfe80 {
                    Some("link-local address")
                } else if v6.segments()[0] & 0xfe00 == 0xfc00 {
                    Some("unique-local address")
                } else {
                    Some("IP-literal host")
                }
            }
        }
    }
}

/// The URL shape checks shared by both egress routes (the artifact route
/// and the private-release `api.github.com` route): https only, no
/// userinfo, the default port only, a non-empty host that is never
/// localhost/intranet or an IP literal. Returns the lowercase host and the
/// path portion. Everything else is rejected with a typed error *before*
/// any network activity.
fn validate_url_common(raw: &str) -> Result<(String, &str), MaterializeError> {
    let reject = |reason: &str| {
        Err(MaterializeError::UnsupportedArtifactUrl {
            url: raw.to_string(),
            reason: reason.to_string(),
        })
    };
    let Some(rest) = raw.strip_prefix("https://") else {
        return reject("only https URLs are accepted");
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.contains('@') {
        return reject("userinfo in the authority is not accepted");
    }
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = match host_port.rsplit_once(':') {
        // Avoid treating a bare IPv6 literal's colons as a port separator.
        Some((h, p)) if !h.contains(':') => (h, Some(p)),
        _ => (host_port, None),
    };
    let host_lower = host.to_ascii_lowercase();
    if let Some(port) = port {
        if port != "443" {
            return reject("non-default ports are not accepted");
        }
    }
    if host_lower.is_empty() {
        return reject("empty host");
    }
    if host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".local")
        || host_lower.ends_with(".internal")
    {
        return reject("localhost or intranet host");
    }
    if is_ip_literal(&host_lower) {
        let reason = ip_literal_reason(&host_lower).unwrap_or("IP-literal host");
        return reject(reason);
    }
    Ok((host_lower, &rest[authority_end..]))
}

/// Validate an artifact URL against the github.com-only egress policy.
///
/// Accepted: `https://github.com/<...>` and the GitHub release-asset CDN
/// hosts, default port only, no userinfo, a non-empty path whose final
/// segment is a file name. Everything else is rejected with a typed error
/// *before* any network activity. The private-release API host
/// ([`API_HOST`]) is deliberately NOT accepted here — it serves the
/// module-constructed fallback route only.
pub fn validate_artifact_url(raw: &str) -> Result<ArtifactUrl, MaterializeError> {
    let (host_lower, path) = validate_url_common(raw)?;
    if !ALLOWED_ARTIFACT_HOSTS.contains(&host_lower.as_str()) {
        return Err(MaterializeError::UnsupportedArtifactUrl {
            url: raw.to_string(),
            reason: format!(
                "host is not github.com, a GitHub release-asset host, or codeload.github.com \
                 (allowed: {})",
                ALLOWED_ARTIFACT_HOSTS.join(", ")
            ),
        });
    }
    // The file name is the final path segment of the raw URL; a trailing
    // slash (or an empty path) means there is no asset to download.
    let file_name = path.rsplit('/').next().unwrap_or_default();
    if file_name.is_empty() {
        return Err(MaterializeError::UnsupportedArtifactUrl {
            url: raw.to_string(),
            reason: "URL has no file name to download".to_string(),
        });
    }
    Ok(ArtifactUrl {
        url: raw.to_string(),
        host: host_lower,
        file_name: file_name.to_string(),
    })
}

/// Validate a URL on the private-release fallback route: https, host
/// exactly [`API_HOST`], and one of the two path shapes this module builds
/// from validated components — `/repos/<owner>/<repo>/releases/tags/<tag>`
/// (release lookup by tag) and `/repos/<owner>/<repo>/releases/assets/<id>`
/// (asset download by numeric id). This is the only place `api.github.com`
/// is an allowed host, and the shape check keeps even this route from
/// becoming a generic egress.
pub fn validate_api_url(raw: &str) -> Result<ArtifactUrl, MaterializeError> {
    let (host_lower, path) = validate_url_common(raw)?;
    if host_lower != API_HOST {
        return Err(MaterializeError::UnsupportedArtifactUrl {
            url: raw.to_string(),
            reason: format!(
                "host is not {API_HOST}; the private-release fallback route only talks to \
                 the GitHub REST API host"
            ),
        });
    }
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let ok = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    );
    let file_name = match ok {
        (
            Some("repos"),
            Some(owner),
            Some(repo),
            Some("releases"),
            Some("tags"),
            Some(tag),
            None,
        ) if is_safe_repo_segment(owner)
            && is_safe_repo_segment(repo)
            && !tag.is_empty()
            && tag != "."
            && tag != ".." =>
        {
            tag.to_string()
        }
        (
            Some("repos"),
            Some(owner),
            Some(repo),
            Some("releases"),
            Some("assets"),
            Some(id),
            None,
        ) if is_safe_repo_segment(owner)
            && is_safe_repo_segment(repo)
            && !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit()) =>
        {
            id.to_string()
        }
        _ => {
            return Err(MaterializeError::UnsupportedArtifactUrl {
                url: raw.to_string(),
                reason: "the private-release fallback route only accepts the module-built \
                     /repos/<owner>/<repo>/releases/tags/<tag> and /releases/assets/<id> paths"
                    .to_string(),
            })
        }
    };
    Ok(ArtifactUrl {
        url: raw.to_string(),
        host: host_lower,
        file_name,
    })
}

/// A `github.com/<owner>/<repo>` coordinate, the source of the tarball the
/// fallback fetches from `codeload.github.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCoordinate {
    pub owner: String,
    pub repo: String,
}

/// Whether a path segment is a safe GitHub owner/repo name: the GitHub
/// charset, and never `.`/`..` (which could smuggle path traversal into the
/// codeload URL).
fn is_safe_repo_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Parse a `github.com` URL into an owner/repo coordinate.
///
/// Accepts both artifact URLs (`https://github.com/<owner>/<repo>/releases/...`
/// — the owner/repo are the first two path segments, so a dead asset URL
/// still carries the coordinate) and clone URLs
/// (`https://github.com/<owner>/<repo>` with an optional `.git` suffix and
/// trailing slash). https only, the host must be exactly `github.com`, no
/// userinfo, no port, no IP literal — the same egress shape the artifact
/// validation enforces. Returns `None` for anything else.
pub fn parse_github_repo(raw: &str) -> Option<RepoCoordinate> {
    let rest = raw.strip_prefix("https://")?;
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.contains('@') || authority.contains(':') {
        return None;
    }
    if !authority.eq_ignore_ascii_case("github.com") || is_ip_literal(authority) {
        return None;
    }
    let mut segments = rest[authority_end..].split('/').filter(|s| !s.is_empty());
    let owner = segments.next()?;
    let mut repo = segments.next()?.to_string();
    if let Some(stripped) = repo.strip_suffix(".git") {
        repo = stripped.to_string();
    }
    if !is_safe_repo_segment(owner) || !is_safe_repo_segment(&repo) {
        return None;
    }
    Some(RepoCoordinate {
        owner: owner.to_string(),
        repo,
    })
}

/// Whether a record commit is a full 40-hex git sha — the only form safe
/// (and meaningful) to interpolate into a codeload tarball path.
pub fn is_full_commit_sha(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The codeload tarball URL of a repository tree at a commit, validated
/// against the same egress policy as artifact URLs (codeload.github.com is
/// GitHub's tarball host and is on the allowlist).
pub fn codeload_tarball_url(
    coordinate: &RepoCoordinate,
    commit: &str,
) -> Result<ArtifactUrl, MaterializeError> {
    let raw = format!(
        "https://codeload.github.com/{}/{}/tar.gz/{}",
        coordinate.owner, coordinate.repo, commit
    );
    validate_artifact_url(&raw)
}

/// A `github.com/<owner>/<repo>/releases/download/<tag>/<asset>` URL
/// resolved into the coordinates the private-release fallback route needs:
/// the repository, the release tag, and the asset's name in both its
/// percent-decoded form (what the API's asset `name` field carries) and
/// its raw URL segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseDownloadRef {
    /// Owner/repo the release lives on.
    pub coordinate: RepoCoordinate,
    /// The tag segment exactly as it appears in the recorded URL.
    pub tag: String,
    /// The percent-decoded asset name (`name` in the release JSON).
    pub asset_name: String,
    /// The asset's raw URL segment (the possibly percent-encoded form),
    /// the fallback match when the recorded URL named the asset encoded.
    pub asset_segment: String,
    /// The full recorded URL, for error messages.
    pub recorded_url: String,
}

/// Parse a release-download URL into a [`ReleaseDownloadRef`].
///
/// Accepts exactly `https://github.com/<owner>/<repo>/releases/download/
/// <tag>/<asset>` — the host checks mirror `parse_github_repo` (https,
/// no userinfo, no port, no IP literal) and the owner/repo charset is
/// validated; the tag and asset segments are carried verbatim (the tag is
/// percent-encoded again before it is interpolated into the API URL).
/// Returns `None` for any other shape — those URLs never enter the
/// fallback.
pub fn parse_release_download_url(raw: &str) -> Option<ReleaseDownloadRef> {
    let rest = raw.strip_prefix("https://")?;
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.contains('@') || authority.contains(':') {
        return None;
    }
    if !authority.eq_ignore_ascii_case("github.com") || is_ip_literal(authority) {
        return None;
    }
    let mut segments = rest[authority_end..].split('/').filter(|s| !s.is_empty());
    let owner = segments.next()?;
    let repo = segments.next()?;
    if segments.next()? != "releases" || segments.next()? != "download" {
        return None;
    }
    let tag = segments.next()?;
    let asset_segment = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    if !is_safe_repo_segment(owner) || !is_safe_repo_segment(repo) || tag.is_empty() {
        return None;
    }
    Some(ReleaseDownloadRef {
        coordinate: RepoCoordinate {
            owner: owner.to_string(),
            repo: repo.to_string(),
        },
        tag: tag.to_string(),
        asset_name: percent_decode(asset_segment),
        asset_segment: asset_segment.to_string(),
        recorded_url: raw.to_string(),
    })
}

/// Percent-decode a URL path segment: every `%XX` escape becomes its byte,
/// everything else passes through. Lossy UTF-8 — the result is used only
/// for exact-name matching against the release JSON.
fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or_default();
            out.push(u8::from_str_radix(hex, 16).unwrap_or(b'%'));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode a URL path segment for interpolation into an API URL:
/// everything outside the unreserved set (`A-Z a-z 0-9 - . _ ~`) becomes
/// `%XX`, so a tag can never introduce `/`, `?`, `#`, or whitespace into
/// the constructed path.
fn encode_url_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The `GET .../releases/tags/<tag>` URL of the private-release fallback
/// route: the coordinate validated, the tag percent-encoded into one path
/// segment, the result passed through the route's validation.
fn api_releases_tag_url(
    coordinate: &RepoCoordinate,
    tag: &str,
) -> Result<ArtifactUrl, MaterializeError> {
    let raw = format!(
        "https://{API_HOST}/repos/{}/{}/releases/tags/{}",
        coordinate.owner,
        coordinate.repo,
        encode_url_segment(tag)
    );
    validate_api_url(&raw)
}

/// The `GET .../releases/assets/<id>` URL of the private-release fallback
/// route, validated the same way.
fn api_release_asset_url(
    coordinate: &RepoCoordinate,
    asset_id: u64,
) -> Result<ArtifactUrl, MaterializeError> {
    let raw = format!(
        "https://{API_HOST}/repos/{}/{}/releases/assets/{asset_id}",
        coordinate.owner, coordinate.repo
    );
    validate_api_url(&raw)
}

/// The release asset whose exact `name` matches, as `(id, name)`. A pure
/// function over the release JSON so the match is testable offline;
/// returns `None` for absent names and unparsable bodies alike.
fn release_asset_id_by_name(release_json: &str, asset_name: &str) -> Option<(u64, String)> {
    let release: serde_json::Value = serde_json::from_str(release_json).ok()?;
    let assets = release.get("assets")?.as_array()?;
    assets
        .iter()
        .filter_map(|asset| {
            let name = asset.get("name")?.as_str()?;
            let id = asset.get("id")?.as_u64()?;
            (name == asset_name).then(|| (id, name.to_string()))
        })
        .next()
}

/// Extract the next hop from a `Location` header block.
///
/// Pure function over the raw header text curl dumps, so redirect handling
/// is testable offline. Returns `Ok(None)` when no `Location` header is
/// present (the caller decides whether that is valid for the status).
pub fn location_from_headers(headers: &str) -> Option<String> {
    headers
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("location") {
                Some(value.trim().to_string())
            } else {
                None
            }
        })
        .next_back()
}

/// Where a GitHub token may come from: the environment, or `gh auth token`.
/// The token value never appears in command lines, logs, or errors.
fn github_token() -> Option<String> {
    for var in TOKEN_ENV_VARS {
        if let Ok(value) = std::env::var(var) {
            if !value.trim().is_empty() {
                return Some(value.trim().to_string());
            }
        }
    }
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if output.status.success() {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

/// Whether a GitHub token is available to the fetch layer right now (the
/// same sources [`download_artifact`] reads: `GITHUB_TOKEN` / `GH_TOKEN`,
/// then `gh auth token`). Public so dependency-source error guidance can
/// branch on credential availability without touching the token value.
pub fn github_token_present() -> bool {
    github_token().is_some()
}

/// The failure text for a source-fallback tarball fetch that answered a
/// definitive 404/410, split by whether the fetch carried credentials.
///
/// A PRIVATE repository answers 404 to an ANONYMOUS fetch no matter how
/// healthy it is, so an absent token is the first thing the message must
/// name — the executor defect this guards against is a run that holds
/// valid credentials (org CI app installation token) but never passed
/// them to the fetch layer, so the source fallback failed with a
/// misleading "repository was deleted or made private". With a token
/// present, the same status means the token cannot see the repository
/// (wrong installation scope) or the repository/commit is really gone.
pub fn source_tarball_gone_reason(status: u16, token_present: bool) -> String {
    if !token_present {
        return format!(
            "HTTP {status}: the tarball was fetched ANONYMOUSLY (no GITHUB_TOKEN/GH_TOKEN in \
             the environment and no `gh auth token`); a PRIVATE repository answers 404 to an \
             anonymous fetch — give the run a token that can read it (the executor's org CI \
             app installation token) and dispatch again"
        );
    }
    format!(
        "HTTP {status}: no repository tarball is readable at this URL with the available \
         credentials (the token cannot see this repository — check the installation scope — \
         or the repository/commit is gone)"
    )
}

/// Write curl headers carrying credential material to a mode-0600 temp
/// file. Passing headers this way keeps the token off the process argv.
fn stage_private_file(prefix: &str, contents: String) -> Result<PathBuf, MaterializeError> {
    let path = unique_temp(prefix);
    std::fs::write(&path, contents).map_err(|e| MaterializeError::ArtifactDownload {
        url: "<credential setup>".into(),
        reason: format!("failed to stage the auth config: {e}"),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// A temp file path that cannot collide within or across processes.
fn unique_temp(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ))
}

/// Write the curl config carrying the Authorization header to a mode-0600
/// temp file (the artifact route's config: Authorization only).
fn write_auth_config(token: &str) -> Result<PathBuf, MaterializeError> {
    stage_private_file(
        "jumbo-asset-auth",
        format!("header = \"Authorization: Bearer {token}\"\n"),
    )
}

/// Write the curl config of a private-release fallback call: the
/// Authorization header plus the route-specific `Accept` and a User-Agent
/// (the API requires one). Same 0600 staging as the artifact config.
fn write_api_config(
    token: &str,
    accept: &str,
    api_version: bool,
) -> Result<PathBuf, MaterializeError> {
    let mut contents = format!(
        "header = \"Authorization: Bearer {token}\"\nheader = \"Accept: {accept}\"\nheader = \"User-Agent: jumbo-build\"\n"
    );
    if api_version {
        contents.push_str("header = \"X-GitHub-Api-Version: 2022-11-28\"\n");
    }
    stage_private_file("jumbo-asset-api-auth", contents)
}

/// One curl hop: fetch `url` into `dest`, returning the HTTP status and
/// the dumped response headers.
fn curl_hop(
    url: &ArtifactUrl,
    dest: &Path,
    headers: &Path,
    auth_config: Option<&Path>,
) -> Result<(u16, String), MaterializeError> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "--silent",
        "--show-error",
        "--max-time",
        "600",
        "--write-out",
        "%{http_code}",
        "--dump-header",
    ])
    .arg(headers)
    .arg("--output")
    .arg(dest)
    .arg("--url")
    .arg(&url.url);
    if let Some(config) = auth_config {
        cmd.arg("--config").arg(config);
    }
    let output = cmd
        .output()
        .map_err(|e| MaterializeError::ArtifactDownload {
            url: url.url.clone(),
            reason: format!(
                "failed to run curl ({e}); curl is required to download release assets"
            ),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // The stderr text of curl contains no credential material.
        return Err(MaterializeError::ArtifactDownload {
            url: url.url.clone(),
            reason: if stderr.is_empty() {
                format!("curl exited with status {}", output.status)
            } else {
                stderr
            },
        });
    }
    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let code = status
        .parse::<u16>()
        .map_err(|_| MaterializeError::ArtifactDownload {
            url: url.url.clone(),
            reason: format!("curl returned no HTTP status (got `{status}`)"),
        })?;
    let header_text =
        std::fs::read_to_string(headers).map_err(|e| MaterializeError::ArtifactDownload {
            url: url.url.clone(),
            reason: format!("failed to read the response headers: {e}"),
        })?;
    Ok((code, header_text))
}

/// Drive the redirect loop for one download: fetch `initial` into `dest`,
/// following 3xx hops one at a time through the same validation, so a
/// redirect can never widen the egress surface. The body of each
/// intermediate hop is discarded; only the final 200 response lands in
/// `dest`. On a definitive 404/410 the producing hop index and status are
/// recorded in `gone` (hop 0 = the initial URL itself) so the caller can
/// tell an absent recorded URL from a dead redirect hop.
fn follow_hops(
    initial: &ArtifactUrl,
    dest: &Path,
    headers_tmp: &Path,
    auth_config: Option<&Path>,
    gone: &mut Option<(usize, u16)>,
) -> Result<(), MaterializeError> {
    let mut current = initial.clone();
    for hop in 0..=MAX_REDIRECTS {
        let (code, header_text) = curl_hop(&current, dest, headers_tmp, auth_config)?;
        if (300..400).contains(&code) {
            let location = location_from_headers(&header_text).ok_or_else(|| {
                MaterializeError::ArtifactDownload {
                    url: current.url.clone(),
                    reason: format!("redirect status {code} without a Location header"),
                }
            })?;
            if location.starts_with('/') || !location.contains("://") {
                return Err(MaterializeError::ArtifactDownload {
                    url: current.url.clone(),
                    reason: format!(
                        "relative redirect `{location}` is not supported; expected an absolute URL"
                    ),
                });
            }
            current = ArtifactUrl::hop(&location)?;
            continue;
        }
        if code == 200 {
            return Ok(());
        }
        // 404/410 are definitive: the release asset is gone at the
        // recorded URL (deleted or re-published under another tag) — or,
        // on the first hop of a PRIVATE repository's asset URL, answered
        // the way GitHub answers unauthorized asset requests (see the
        // fallback in `download_artifact`). They carry a dedicated
        // variant so the dependency ingestion path can fall back to
        // source materialization, while every other status (5xx outages,
        // auth failures, ...) stays a hard download error — real outages
        // must remain visible.
        if code == 404 || code == 410 {
            *gone = Some((hop, code));
            return Err(MaterializeError::ArtifactGone {
                url: current.url.clone(),
                status: code,
            });
        }
        return Err(MaterializeError::ArtifactDownload {
            url: current.url.clone(),
            reason: match code {
                401 | 403 => format!(
                    "HTTP {code}: authentication required or insufficient; set GITHUB_TOKEN, \
                     GH_TOKEN, or run `gh auth login` (credentials are never stored by jumbo)"
                ),
                other => format!("HTTP {other}"),
            },
        });
    }
    Err(MaterializeError::ArtifactDownload {
        url: initial.url.clone(),
        reason: format!("more than {MAX_REDIRECTS} redirect hops"),
    })
}

/// Resolve a private release asset through the authenticated api.github.com
/// route: look the release up by tag, match the asset by exact name, then
/// download it via `GET /releases/assets/<id>` with
/// `Accept: application/octet-stream` — following the redirect to the
/// release-asset CDN through the same validated hop loop as a public
/// download. The token is handed to curl only through mode-0600 config
/// files and every URL is built from validated components.
///
/// Error mapping: a 404 from the API (release or asset genuinely absent)
/// becomes [`MaterializeError::ArtifactGone`] so the dependency ingestion
/// path keeps falling back to source exactly as before; an API failure
/// (network, 5xx, insufficient token) becomes a hard
/// [`MaterializeError::ArtifactDownload`] — auth problems must stay
/// visible, never collapse into "asset gone".
fn fetch_release_asset_via_api(
    dl: &ReleaseDownloadRef,
    dest: &Path,
    token: &str,
) -> Result<(), MaterializeError> {
    let json_config = write_api_config(token, "application/vnd.github+json", true)?;
    let json_body = unique_temp("jumbo-asset-api-release");
    let json_headers = unique_temp("jumbo-asset-api-headers");
    let failure = |reason: String| MaterializeError::ArtifactDownload {
        url: dl.recorded_url.clone(),
        reason,
    };
    let result = (|| -> Result<(), MaterializeError> {
        let tags_url = api_releases_tag_url(&dl.coordinate, &dl.tag)?;
        let (code, _) = curl_hop(&tags_url, &json_body, &json_headers, Some(&json_config))?;
        if code == 404 {
            // The API 404s for a release the token cannot see — for a
            // private asset that IS the definitive "no such asset here".
            return Err(MaterializeError::ArtifactGone {
                url: dl.recorded_url.clone(),
                status: 404,
            });
        }
        if code != 200 {
            return Err(failure(format!(
                "the private-release asset lookup via {API_HOST} answered HTTP {code}; the \
                 token must be able to read the release that carries the asset"
            )));
        }
        let body = std::fs::read_to_string(&json_body).map_err(|e| {
            failure(format!(
                "failed to read the {API_HOST} release response: {e}"
            ))
        })?;
        let asset = release_asset_id_by_name(&body, &dl.asset_name)
            .or_else(|| release_asset_id_by_name(&body, &dl.asset_segment));
        let Some((asset_id, _)) = asset else {
            return Err(MaterializeError::ArtifactGone {
                url: dl.recorded_url.clone(),
                status: 404,
            });
        };
        let asset_url = api_release_asset_url(&dl.coordinate, asset_id)?;
        let asset_config = write_api_config(token, "application/octet-stream", false)?;
        let asset_headers = unique_temp("jumbo-asset-api-headers");
        let mut gone_hop = None;
        let download = follow_hops(
            &asset_url,
            dest,
            &asset_headers,
            Some(&asset_config),
            &mut gone_hop,
        );
        let _ = std::fs::remove_file(&asset_headers);
        let _ = std::fs::remove_file(&asset_config);
        download
    })();
    let _ = std::fs::remove_file(&json_body);
    let _ = std::fs::remove_file(&json_headers);
    let _ = std::fs::remove_file(&json_config);
    result
}

/// Download the asset at the validated URL into `dest` (a file path).
///
/// Redirects are followed one hop at a time; every hop is re-validated
/// against the same github.com-only policy before it is fetched, so a
/// redirect can never widen the egress surface.
///
/// **Private-release fallback**: on a PRIVATE repository the recorded
/// `github.com/<o>/<r>/releases/download/...` URL answers 404 even with an
/// Authorization header — GitHub serves private release assets only
/// through the authenticated api.github.com asset route. When the FIRST
/// hop (the recorded URL itself) answers 404 and a token is present, the
/// asset is resolved through that route ([`fetch_release_asset_via_api`])
/// before the failure is reported. A public asset 302s from the recorded
/// URL and never enters the fallback: the public path is byte-compatible.
pub fn download_artifact(url: &ArtifactUrl, dest: &Path) -> Result<(), MaterializeError> {
    let token = github_token();
    let auth_config = token.as_deref().map(write_auth_config).transpose()?;
    let headers_tmp = unique_temp("jumbo-asset-headers");

    let mut gone: Option<(usize, u16)> = None;
    let mut result = follow_hops(url, dest, &headers_tmp, auth_config.as_deref(), &mut gone);

    // The diagnosed defect (jumbo-publish dedup re-pull on private member
    // repositories): a 404 ON THE FIRST HOP — the recorded URL itself —
    // with a token available means "private asset, use the API route", not
    // "gone" (GitHub answers 404 for a private repository's asset URL even
    // with an Authorization header; a 410 stays a definitive absence).
    if gone == Some((0, 404)) {
        if let Some(token_value) = token.as_deref() {
            if let Some(dl) = parse_release_download_url(&url.url) {
                match fetch_release_asset_via_api(&dl, dest, token_value) {
                    Ok(()) => result = Ok(()),
                    Err(err) => result = Err(err),
                }
            }
        }
    }

    let _ = std::fs::remove_file(&headers_tmp);
    if let Some(config) = &auth_config {
        let _ = std::fs::remove_file(config);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_github_hosts_only() {
        for ok in [
            "https://github.com/acme/pkg/releases/download/v2.4.0/pkg-2.4.0-py3-none-any.whl",
            "https://GITHUB.COM/acme/pkg/releases/download/v1.0.0/a.tgz",
            "https://objects.githubusercontent.com/releases/1234/pkg.whl",
            "https://release-assets.githubusercontent.com/1234/pkg.whl",
            "https://github.com:443/acme/pkg/releases/download/v1.0.0/a.whl",
        ] {
            let url = validate_artifact_url(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert!(ALLOWED_ARTIFACT_HOSTS.contains(&url.host.as_str()));
        }
    }

    #[test]
    fn rejects_non_github_and_unsafe_hosts() {
        for bad in [
            "http://github.com/acme/pkg/releases/download/v1/a.whl",
            "https://evil.com/pkg.whl",
            "https://github.com.evil.io/pkg.whl",
            "https://localhost/pkg.whl",
            "https://127.0.0.1/pkg.whl",
            "https://[::1]/pkg.whl",
            "https://10.0.0.5/pkg.whl",
            "https://192.168.1.4/pkg.whl",
            "https://169.254.169.254/latest/meta-data",
            "https://[fe80::1]/pkg.whl",
            "https://[fc00::1]/pkg.whl",
            "https://user:token@github.com/acme/pkg/releases/download/v1/a.whl",
            "https://github.com:8443/acme/pkg/releases/download/v1/a.whl",
            "https://github.com",
            "https://github.com/acme/pkg/releases/download/",
            "ftp://github.com/a.whl",
            "file:///etc/passwd",
        ] {
            let err = validate_artifact_url(bad)
                .err()
                .unwrap_or_else(|| panic!("`{bad}` should be rejected"));
            assert!(
                err.to_string().contains("not allowed"),
                "`{bad}` error should name the policy: {err}"
            );
        }
    }

    #[test]
    fn file_name_is_the_final_path_segment() {
        let url = validate_artifact_url(
            "https://github.com/acme/pkg/releases/download/v2.4.0/pkg-2.4.0-py3-none-any.whl",
        )
        .expect("valid");
        assert_eq!(url.file_name, "pkg-2.4.0-py3-none-any.whl");
    }

    #[test]
    fn location_parsing_takes_the_last_header_case_insensitively() {
        let headers = "HTTP/2 302\r\ncontent-length: 0\r\nlocation: https://a.example/x\r\nLOCATION: https://github.com/acme/y\r\n\r\n";
        assert_eq!(
            location_from_headers(headers).as_deref(),
            Some("https://github.com/acme/y")
        );
        assert_eq!(location_from_headers("HTTP/2 200\r\n\r\n"), None);
    }

    #[test]
    fn codeload_tarball_urls_pass_validation() {
        let coordinate = RepoCoordinate {
            owner: "acme".into(),
            repo: "demo-gamma".into(),
        };
        let url = codeload_tarball_url(&coordinate, "f00dcafe0123456789abcdef0123456789abcdef0")
            .expect("valid codeload URL");
        assert_eq!(
            url.url,
            "https://codeload.github.com/acme/demo-gamma/tar.gz/f00dcafe0123456789abcdef0123456789abcdef0"
        );
        assert_eq!(url.host, "codeload.github.com");
        // The commit is the final path segment: the staged file name a
        // cache provider resolves the tarball by.
        assert_eq!(url.file_name, "f00dcafe0123456789abcdef0123456789abcdef0");
    }

    #[test]
    fn source_tarball_404_names_the_missing_credentials_first() {
        // The diagnosed executor defect: the run HELD valid credentials
        // (org CI app installation token) but the fetch layer saw none,
        // so a healthy private repository's codeload tarball "404'd". The
        // anonymous message must say the fetch was anonymous and how to
        // fix it — not "the repository was deleted".
        let anonymous = source_tarball_gone_reason(404, false);
        assert!(anonymous.contains("HTTP 404"), "{anonymous}");
        assert!(anonymous.contains("ANONYMOUSLY"), "{anonymous}");
        assert!(
            anonymous.contains("GITHUB_TOKEN") && anonymous.contains("GH_TOKEN"),
            "{anonymous}"
        );
        assert!(
            anonymous.contains("installation token"),
            "the anonymous message must point at the executor credential: {anonymous}"
        );
        // The same split holds for a definitive 410.
        assert!(source_tarball_gone_reason(410, false).contains("HTTP 410"));
        // With a token present the same status is a scope/visibility
        // problem instead — named as such, never as an anonymous fetch.
        let authenticated = source_tarball_gone_reason(404, true);
        assert!(authenticated.contains("HTTP 404"), "{authenticated}");
        assert!(
            authenticated.contains("cannot see this repository"),
            "{authenticated}"
        );
        assert!(
            !authenticated.to_lowercase().contains("anonymously"),
            "{authenticated}"
        );
    }

    #[test]
    fn repo_coordinates_parse_from_artifact_and_clone_urls() {
        // A release-asset URL (even a dead one) carries owner/repo as its
        // first two path segments.
        assert_eq!(
            parse_github_repo(
                "https://github.com/acme/demo-gamma/releases/download/v1.0.0/demo_gamma-1.0.0.whl"
            ),
            Some(RepoCoordinate {
                owner: "acme".into(),
                repo: "demo-gamma".into()
            })
        );
        // Clone URL forms: bare, .git suffix, trailing slash, mixed case host.
        for raw in [
            "https://github.com/acme/demo-gamma",
            "https://github.com/acme/demo-gamma.git",
            "https://github.com/acme/demo-gamma/",
            "https://GITHUB.COM/acme/demo-gamma",
        ] {
            assert_eq!(
                parse_github_repo(raw),
                Some(RepoCoordinate {
                    owner: "acme".into(),
                    repo: "demo-gamma".into()
                }),
                "{raw}"
            );
        }
        // Anything else is not a coordinate.
        for raw in [
            "https://evil.com/acme/pkg",
            "https://github.com.evil.io/acme/pkg",
            "http://github.com/acme/pkg",
            "https://user@github.com/acme/pkg",
            "https://github.com:8443/acme/pkg",
            "https://github.com",
            "https://github.com/acme",
            "https://github.com/acme/../etc",
            "ssh://git@github.com/acme/pkg",
            "git@github.com:acme/pkg.git",
        ] {
            assert_eq!(parse_github_repo(raw), None, "{raw}");
        }
    }

    #[test]
    fn commit_validation_only_accepts_full_shas() {
        assert!(is_full_commit_sha(
            "f00dcafe0123456789abcdef0123456789abcdef"
        ));
        assert!(is_full_commit_sha(&"a".repeat(40)));
        assert!(!is_full_commit_sha("f00dcafe"));
        assert!(!is_full_commit_sha("v1.0.0"));
        assert!(!is_full_commit_sha("HEAD"));
        assert!(!is_full_commit_sha("../escape"));
        assert!(!is_full_commit_sha(&"a".repeat(41)));
        assert!(!is_full_commit_sha(&"g".repeat(40)));
    }

    // ---- The private-release fallback route (api.github.com) ----

    const PRIVATE_URL: &str =
        "https://github.com/acme/private-pkg/releases/download/v2.4.0/demo_alpha-2.4.0-py3-none-any.whl";

    #[test]
    fn artifact_urls_still_reject_the_api_host() {
        // Route separation: api.github.com serves the module-built
        // fallback route only. A recorded artifact URL pointing there is
        // rejected exactly like any other disallowed host.
        let err = validate_artifact_url(
            "https://api.github.com/repos/acme/private-pkg/releases/assets/12345",
        )
        .expect_err("api.github.com is not an artifact host");
        assert!(err.to_string().contains("not allowed"), "got: {err}");
    }

    #[test]
    fn api_route_accepts_only_its_own_urls() {
        let coordinate = RepoCoordinate {
            owner: "acme".into(),
            repo: "private-pkg".into(),
        };
        let tags = api_releases_tag_url(&coordinate, "v2.4.0").expect("tags URL");
        assert_eq!(tags.host, "api.github.com");
        assert_eq!(
            tags.url,
            "https://api.github.com/repos/acme/private-pkg/releases/tags/v2.4.0"
        );
        let asset = api_release_asset_url(&coordinate, 123456).expect("asset URL");
        assert_eq!(
            asset.url,
            "https://api.github.com/repos/acme/private-pkg/releases/assets/123456"
        );
        // A tag that needs encoding stays one path segment and round-trips
        // through the route validation.
        let encoded = api_releases_tag_url(&coordinate, "release/1.0 beta+2").expect("encoded");
        assert_eq!(
            encoded.url,
            "https://api.github.com/repos/acme/private-pkg/releases/tags/release%2F1.0%20beta%2B2"
        );

        for bad in [
            "http://api.github.com/repos/acme/pkg/releases/tags/v1",
            "https://api.github.com.evil.io/repos/acme/pkg/releases/tags/v1",
            "https://api-github.com/repos/acme/pkg/releases/tags/v1",
            "https://api.github.com:8443/repos/acme/pkg/releases/tags/v1",
            "https://user:token@api.github.com/repos/acme/pkg/releases/tags/v1",
            "https://127.0.0.1/repos/acme/pkg/releases/tags/v1",
            "https://api.github.com/repos/../etc/releases/tags/v1",
            "https://api.github.com/repos/acme/pkg/git/tags/v1",
            "https://api.github.com/repos/acme/pkg/releases/tags/v1/extra",
            "https://api.github.com/repos/acme/pkg/releases/assets/12ab",
            "https://api.github.com/repos/acme/pkg/releases/assets/",
            "https://api.github.com/repos/acme/pkg/releases/tags/",
            "https://api.github.com/user",
            "https://github.com/acme/pkg/releases/tags/v1",
        ] {
            let err = validate_api_url(bad)
                .err()
                .unwrap_or_else(|| panic!("`{bad}` should be rejected"));
            assert!(
                err.to_string().contains("not allowed"),
                "`{bad}` error should name the policy: {err}"
            );
        }
    }

    #[test]
    fn release_download_urls_parse_with_decoded_names() {
        let dl = parse_release_download_url(PRIVATE_URL).expect("parses");
        assert_eq!(dl.coordinate.owner, "acme");
        assert_eq!(dl.coordinate.repo, "private-pkg");
        assert_eq!(dl.tag, "v2.4.0");
        assert_eq!(dl.asset_name, "demo_alpha-2.4.0-py3-none-any.whl");

        // A percent-encoded asset name decodes into the API's `name` form,
        // and the raw segment is carried for the fallback match.
        let dl = parse_release_download_url(
            "https://github.com/acme/private-pkg/releases/download/v1.0.0/demo%20kit-1.0.0.tgz",
        )
        .expect("parses");
        assert_eq!(dl.asset_name, "demo kit-1.0.0.tgz");
        assert_eq!(dl.asset_segment, "demo%20kit-1.0.0.tgz");

        // Only the exact release-download shape enters the fallback.
        for other in [
            "https://github.com/acme/private-pkg/releases/tag/v1.0.0",
            "https://github.com/acme/private-pkg/releases/download/v1.0.0",
            "https://github.com/acme/private-pkg/releases/download/",
            "https://github.com/acme/private-pkg/blob/main/README.md",
            "https://github.com/acme/private-pkg",
            "https://github.com/acme/private-pkg/releases/download/v1/a/b.whl",
            "https://objects.githubusercontent.com/releases/1234/pkg.whl",
            "https://user:token@github.com/acme/pkg/releases/download/v1/a.whl",
            "https://github.com:8443/acme/pkg/releases/download/v1/a.whl",
            "http://github.com/acme/pkg/releases/download/v1/a.whl",
            "https://github.com.acme.io/pkg/releases/download/v1/a.whl",
            "https://github.com/acme/../etc/releases/download/v1/a.whl",
        ] {
            assert_eq!(
                parse_release_download_url(other),
                None,
                "`{other}` must not enter the fallback"
            );
        }
    }

    #[test]
    fn segment_encoding_and_decoding_round_trip() {
        for segment in [
            "v1.0.0",
            "release/1.0 beta+2",
            "demo kit-1.0.0.tgz",
            "héllo~wörld.tar.gz",
            "a?b#c%d",
        ] {
            let encoded = encode_url_segment(segment);
            // The encoded form must contain no raw path-hostile characters
            // and decode back to the original.
            assert!(!encoded.contains(' '), "{segment}: {encoded}");
            assert!(!encoded.contains('/'), "{segment}: {encoded}");
            assert!(!encoded.contains('?'), "{segment}: {encoded}");
            assert!(!encoded.contains('#'), "{segment}: {encoded}");
            assert_eq!(percent_decode(&encoded), segment, "{segment}");
        }
        assert_eq!(percent_decode("plain-name.whl"), "plain-name.whl");
        assert_eq!(percent_decode("trailing%"), "trailing%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn release_json_matches_the_exact_asset_name() {
        let release = r#"{
          "id": 1001, "tag_name": "v2.4.0",
          "assets": [
            { "id": 2001, "name": "SHA256SUMS" },
            { "id": 2002, "name": "demo_alpha-2.4.0-py3-none-any.whl" },
            { "id": 2003, "name": "demo_alpha-2.4.0.tar.gz" }
          ]
        }"#;
        assert_eq!(
            release_asset_id_by_name(release, "demo_alpha-2.4.0-py3-none-any.whl"),
            Some((2002, "demo_alpha-2.4.0-py3-none-any.whl".into()))
        );
        // Exact matching: no prefix, suffix, or case-insensitive hit.
        assert_eq!(release_asset_id_by_name(release, "demo_alpha-2.4.0"), None);
        assert_eq!(
            release_asset_id_by_name(release, "DEMO_ALPHA-2.4.0-PY3-NONE-ANY.WHL"),
            None
        );
        assert_eq!(release_asset_id_by_name("not json", "x"), None);
        assert_eq!(release_asset_id_by_name(r#"{"assets": []}"#, "x"), None);
    }
}
