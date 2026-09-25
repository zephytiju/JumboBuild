//! The validated github.com-only artifact fetch layer
//! (Jumbo Build & Versioning Standard, §3.4 Artifact Storage and
//! Materialization).
//!
//! JumboBuild downloads release assets **by exact URL** and verifies the
//! recorded SHA-256; no registry protocol (pip, npm) is ever contacted for
//! an internal package. The layer is deliberately narrow:
//!
//! - `https` only, and the host must be one of `github.com` (where release
//!   asset URLs live) or GitHub's release-asset CDN hosts
//!   (`objects.githubusercontent.com`, `release-assets.githubusercontent.com`,
//!   to where a download redirects). Everything else — other hosts,
//!   localhost, loopback/private/reserved addresses, IP literals, userinfo,
//!   and non-default ports — is rejected before any bytes move.
//! - Redirects are followed manually, one hop at a time through this same
//!   validation, so a redirect can never widen the egress surface.
//! - Credentials come only from the environment (`GITHUB_TOKEN`,
//!   `GH_TOKEN`) or from `gh auth token`. The token is handed to curl via
//!   a mode-0600 config file, never on the command line (where `ps` could
//!   read it), never in a log line, and never in an error message.
//!
//! The host rules are pure functions so they are fully testable offline;
//! `download_artifact` itself is a thin curl driver over them.

use std::path::{Path, PathBuf};

use super::error::MaterializeError;

/// Hosts an artifact URL (and every redirect hop) may point at.
pub const ALLOWED_ARTIFACT_HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Environment variables a GitHub token is accepted from, in order.
pub const TOKEN_ENV_VARS: [&str; 2] = ["GITHUB_TOKEN", "GH_TOKEN"];

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

/// Validate an artifact URL against the github.com-only egress policy.
///
/// Accepted: `https://github.com/<...>` and the GitHub release-asset CDN
/// hosts, default port only, no userinfo, a non-empty path whose final
/// segment is a file name. Everything else is rejected with a typed error
/// *before* any network activity.
pub fn validate_artifact_url(raw: &str) -> Result<ArtifactUrl, MaterializeError> {
    let reject = |reason: &str| {
        Err(MaterializeError::UnsupportedArtifactUrl {
            url: raw.to_string(),
            reason: reason.to_string(),
        })
    };
    let reject_owned = |reason: String| {
        Err(MaterializeError::UnsupportedArtifactUrl {
            url: raw.to_string(),
            reason,
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
    if !ALLOWED_ARTIFACT_HOSTS.contains(&host_lower.as_str()) {
        return reject_owned(format!(
            "host is not github.com or a GitHub release-asset host (allowed: {})",
            ALLOWED_ARTIFACT_HOSTS.join(", ")
        ));
    }
    let path = &rest[authority_end..];
    // The file name is the final path segment of the raw URL; a trailing
    // slash (or an empty path) means there is no asset to download.
    let file_name = path.rsplit('/').next().unwrap_or_default();
    if file_name.is_empty() {
        return reject("URL has no file name to download");
    }
    Ok(ArtifactUrl {
        url: raw.to_string(),
        host: host_lower,
        file_name: file_name.to_string(),
    })
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

/// Write the curl config carrying the Authorization header to a mode-0600
/// temp file. Passing the token this way keeps it off the process argv.
fn write_auth_config(token: &str) -> Result<PathBuf, MaterializeError> {
    let path = std::env::temp_dir().join(format!(
        "jumbo-asset-auth-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::write(
        &path,
        format!("header = \"Authorization: Bearer {token}\"\n"),
    )
    .map_err(|e| MaterializeError::ArtifactDownload {
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

/// Download the asset at the validated URL into `dest` (a file path).
///
/// Redirects are followed one hop at a time; every hop is re-validated
/// against the same github.com-only policy before it is fetched, so a
/// redirect can never widen the egress surface. The body of each
/// intermediate hop is discarded; only the final 200 response lands in
/// `dest`.
pub fn download_artifact(url: &ArtifactUrl, dest: &Path) -> Result<(), MaterializeError> {
    let token = github_token();
    let auth_config = token.as_deref().map(write_auth_config).transpose()?;
    let headers_tmp = std::env::temp_dir().join(format!(
        "jumbo-asset-headers-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));

    let mut current = url.clone();
    let result = (|| -> Result<(), MaterializeError> {
        for _ in 0..=MAX_REDIRECTS {
            let (code, header_text) =
                curl_hop(&current, dest, &headers_tmp, auth_config.as_deref())?;
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
            // recorded URL (deleted or re-published under another tag).
            // They carry a dedicated variant so the dependency ingestion
            // path can fall back to source materialization, while every
            // other status (5xx outages, auth failures, ...) stays a hard
            // download error — real outages must remain visible.
            if code == 404 || code == 410 {
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
            url: url.url.clone(),
            reason: format!("more than {MAX_REDIRECTS} redirect hops"),
        })
    })();

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
}
