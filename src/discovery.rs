use crate::config::Config;
use crate::process::{run_step, Step};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub name: String,
    pub version: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    pub description: Option<String>,
    /// Repository the source itself names (`core`, `extra`, `AUR`).
    #[serde(default)]
    pub repository: Option<String>,
    /// Maintainer or packager as the source states it.
    #[serde(default)]
    pub maintainer: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub architecture: Option<String>,
    /// Size string exactly as the source reports it.
    #[serde(default)]
    pub size: Option<String>,
    /// AUR's own "this may no longer build" marker, set to the date it was
    /// flagged. It changes which source is safest, so it must reach the UI.
    #[serde(default)]
    pub out_of_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionRow {
    pub source: String,
    pub availability: String, // "available", "unavailable", "unknown", "disabled"
    pub recommended: bool,
    pub reason: String,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionReport {
    pub target: String,
    pub recommended_source: Option<String>,
    pub decision_table: Vec<DecisionRow>,
}

pub fn search(target: &str, explicit_repo: Option<&str>, config: &Config) -> DecisionReport {
    let sources = [
        "official", "aur", "flatpak", "appimage", "upstream", "deb", "rpm",
    ];
    let mut rows = Vec::new();

    let is_deb_file = target.ends_with(".deb") && Path::new(target).exists();
    let is_rpm_file = target.ends_with(".rpm") && Path::new(target).exists();

    // Determine repository URL if available
    let repo_url = explicit_repo
        .map(|s| s.to_string())
        .or_else(|| config.repos.get(target).cloned());

    for source in sources {
        if !config.is_enabled(source) {
            rows.push(DecisionRow {
                source: source.to_string(),
                availability: "disabled".to_string(),
                recommended: false,
                reason: format!(
                    "Source '{}' is disabled in configuration preferences",
                    source
                ),
                candidates: vec![],
            });
            continue;
        }

        match source {
            "official" => {
                let row = query_official(target);
                rows.push(row);
            }
            "aur" => {
                let row = query_aur(target);
                rows.push(row);
            }
            "flatpak" => {
                let row = query_release_asset(target, repo_url.as_deref(), "flatpak");
                rows.push(row);
            }
            "appimage" => {
                let row = query_release_asset(target, repo_url.as_deref(), "appimage");
                rows.push(row);
            }
            "upstream" => {
                let row = query_upstream_source(target, repo_url.as_deref());
                rows.push(row);
            }
            "deb" => {
                let availability = if is_deb_file {
                    "available"
                } else {
                    "unavailable"
                };
                let reason = if is_deb_file {
                    "Local DEB file detected for inspection"
                } else {
                    "No DEB package specified or file not found"
                };
                let candidates = if is_deb_file {
                    vec![Candidate {
                        name: target.to_string(),
                        version: None,
                        url: Some(target.to_string()),
                        source_url: None,
                        description: Some("Local DEB package file (inspection path)".to_string()),
                        ..Default::default()
                    }]
                } else {
                    vec![]
                };
                rows.push(DecisionRow {
                    source: source.to_string(),
                    availability: availability.to_string(),
                    recommended: false,
                    reason: reason.to_string(),
                    candidates,
                });
            }
            "rpm" => {
                let availability = if is_rpm_file {
                    "available"
                } else {
                    "unavailable"
                };
                let reason = if is_rpm_file {
                    "Local RPM file detected for inspection"
                } else {
                    "No RPM package specified or file not found"
                };
                let candidates = if is_rpm_file {
                    vec![Candidate {
                        name: target.to_string(),
                        version: None,
                        url: Some(target.to_string()),
                        source_url: None,
                        description: Some("Local RPM package file (inspection path)".to_string()),
                        ..Default::default()
                    }]
                } else {
                    vec![]
                };
                rows.push(DecisionRow {
                    source: source.to_string(),
                    availability: availability.to_string(),
                    recommended: false,
                    reason: reason.to_string(),
                    candidates,
                });
            }
            _ => {}
        }
    }

    // Determine recommendation: first available source in preference order
    let mut recommended_source = None;
    for row in &mut rows {
        if row.availability == "available" && recommended_source.is_none() {
            row.recommended = true;
            recommended_source = Some(row.source.clone());
        }
    }

    DecisionReport {
        target: target.to_string(),
        recommended_source,
        decision_table: rows,
    }
}

fn query_official(target: &str) -> DecisionRow {
    let step = Step::new(
        "pacman",
        vec!["-Ss", target],
        "Query official Arch repositories",
        10,
    );
    match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            let mut candidates = parse_pacman_ss(&out.stdout, target);
            enrich_official(&mut candidates);
            if candidates.is_empty() {
                DecisionRow {
                    source: "official".to_string(),
                    availability: "unavailable".to_string(),
                    recommended: false,
                    reason: format!("No exact/matching official package found for '{}'", target),
                    candidates: vec![],
                }
            } else {
                DecisionRow {
                    source: "official".to_string(),
                    availability: "available".to_string(),
                    recommended: false,
                    reason: format!(
                        "Found {} candidate(s) in official Arch repositories",
                        candidates.len()
                    ),
                    candidates,
                }
            }
        }
        Ok(_) => DecisionRow {
            source: "official".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: format!("No official repository package matched '{}'", target),
            candidates: vec![],
        },
        Err(e) => DecisionRow {
            source: "official".to_string(),
            availability: "unknown".to_string(),
            recommended: false,
            reason: format!("Failed to query pacman sync db: {}", e),
            candidates: vec![],
        },
    }
}

fn parse_pacman_ss(output: &str, target: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if !line.is_empty() && line.contains('/') {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let pkg_full = parts[0]; // e.g. core/bash or extra/bash
                let version = parts[1];
                let pkg_name = pkg_full.split('/').nth(1).unwrap_or(pkg_full);
                let desc = if i + 1 < lines.len() {
                    lines[i + 1].trim().to_string()
                } else {
                    String::new()
                };
                if pkg_name.eq_ignore_ascii_case(target) || pkg_name.contains(target) {
                    candidates.push(Candidate {
                        name: pkg_name.to_string(),
                        version: Some(version.to_string()),
                        url: None,
                        source_url: None,
                        description: if desc.is_empty() { None } else { Some(desc) },
                        repository: if pkg_full.contains('/') {
                            Some(pkg_full.split('/').next().unwrap_or("").to_string())
                        } else {
                            None
                        },
                        ..Default::default()
                    });
                }
            }
        }
        i += 1;
    }
    candidates
}

/// `pacman -Ss` yields only names, versions and one-line descriptions, so the
/// detail panel used to fill licence, architecture, size and maintainer with
/// invented text. `-Si` reports all of them from the sync database, and only the
/// leading candidates are worth the extra local lookups.
fn enrich_official(candidates: &mut [Candidate]) {
    for candidate in candidates.iter_mut().take(3) {
        let selector = match &candidate.repository {
            Some(repo) if !repo.is_empty() => format!("{}/{}", repo, candidate.name),
            _ => candidate.name.clone(),
        };
        let step = Step::new(
            "pacman",
            vec!["-Si", "--color=never", &selector],
            "Read official package metadata",
            10,
        );
        let Ok(out) = run_step(&step, None) else {
            continue;
        };
        if out.exit_code != 0 {
            continue;
        }
        let fields = parse_pacman_si(&out.stdout);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, value)| value.clone())
        };
        if candidate.version.is_none() {
            candidate.version = get("Version");
        }
        candidate.repository = get("Repository").or_else(|| candidate.repository.clone());
        candidate.architecture = get("Architecture");
        candidate.license = get("Licenses");
        candidate.size = get("Installed Size").or_else(|| get("Download Size"));
        candidate.maintainer = get("Maintainer")
            .filter(|m| !m.eq_ignore_ascii_case("Unknown Packager"))
            .or_else(|| get("Packager"));
        if candidate.url.is_none() {
            candidate.url = get("URL");
        }
        if candidate.description.is_none() {
            candidate.description = get("Description");
        }
    }
}

/// Parse `pacman -Si` "Key : Value" records, rejoining the values pacman wraps
/// onto indented continuation lines.
fn parse_pacman_si(output: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = output.lines().collect();
    let mut fields = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((key, value)) = lines[i].split_once(" : ") {
            let mut values = vec![value.trim().to_string()];
            let mut j = i + 1;
            while j < lines.len() && lines[j].starts_with("    ") {
                values.push(lines[j].trim().to_string());
                j += 1;
            }
            let joined = values
                .into_iter()
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !joined.is_empty() {
                fields.push((key.trim().to_string(), joined));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    fields
}

/// Percent-encode a value destined for a URL path or query segment. A search
/// target is user input: an unencoded space, `?`, `#` or `..` silently changes
/// which resource the request actually reaches.
fn encode_url_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

fn query_aur(target: &str) -> DecisionRow {
    let url = format!(
        "https://aur.archlinux.org/rpc/v5/search/{}",
        encode_url_segment(target)
    );
    let step = Step::new(
        "curl",
        vec!["-fsSL", "-m", "10", &url],
        "Query AUR RPC v5",
        10,
    );
    match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&out.stdout) {
                if let Some(results) = val.get("results").and_then(|r| r.as_array()) {
                    let mut candidates = Vec::new();
                    for item in results {
                        let name = item
                            .get("Name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let ver = item
                            .get("Version")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let desc = item
                            .get("Description")
                            .and_then(|d| d.as_str())
                            .map(|s| s.to_string());
                        let aur_url = item
                            .get("URL")
                            .and_then(|u| u.as_str())
                            .map(|s| s.to_string());
                        let maintainer = item
                            .get("Maintainer")
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string());
                        let out_of_date = item
                            .get("OutOfDate")
                            .and_then(|o| o.as_str())
                            .map(|s| s.to_string());
                        // `URLPath` is the AUR git snapshot holding the PKGBUILD —
                        // the actual thing a build would consume.
                        let snapshot_url = item
                            .get("URLPath")
                            .and_then(|u| u.as_str())
                            .map(|path| format!("https://aur.archlinux.org{}", path));

                        if name.eq_ignore_ascii_case(target) || name.contains(target) {
                            candidates.push(Candidate {
                                name,
                                version: ver,
                                url: aur_url,
                                source_url: snapshot_url,
                                description: desc,
                                repository: Some("AUR".to_string()),
                                maintainer,
                                out_of_date,
                                ..Default::default()
                            });
                        }
                    }

                    if candidates.is_empty() {
                        return DecisionRow {
                            source: "aur".to_string(),
                            availability: "unavailable".to_string(),
                            recommended: false,
                            reason: format!("No matching packages found in AUR for '{}'", target),
                            candidates: vec![],
                        };
                    } else {
                        return DecisionRow {
                            source: "aur".to_string(),
                            availability: "available".to_string(),
                            recommended: false,
                            reason: format!("Found {} package(s) in AUR", candidates.len()),
                            candidates,
                        };
                    }
                }
            }
            DecisionRow {
                source: "aur".to_string(),
                availability: "unavailable".to_string(),
                recommended: false,
                reason: "AUR RPC returned invalid JSON or empty result".to_string(),
                candidates: vec![],
            }
        }
        Ok(_) => DecisionRow {
            source: "aur".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: format!("AUR RPC query returned no results for '{}'", target),
            candidates: vec![],
        },
        Err(e) => DecisionRow {
            source: "aur".to_string(),
            availability: "unknown".to_string(),
            recommended: false,
            reason: format!("Failed to reach AUR RPC API: {}", e),
            candidates: vec![],
        },
    }
}

fn query_release_asset(target: &str, repo_url: Option<&str>, asset_type: &str) -> DecisionRow {
    let repo_url_str = match repo_url {
        Some(url) => url,
        None => {
            return DecisionRow {
                source: asset_type.to_string(),
                availability: "unavailable".to_string(),
                recommended: false,
                reason: format!(
                    "No repository URL known for target '{}'; pass --repo <url>",
                    target
                ),
                candidates: vec![],
            };
        }
    };

    let api_url = match parse_github_repo(repo_url_str) {
        Some((owner, repo)) => format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            owner, repo
        ),
        None => {
            return DecisionRow {
                source: asset_type.to_string(),
                availability: "unavailable".to_string(),
                recommended: false,
                reason: format!(
                    "Repository URL '{}' is not a recognized GitHub release host",
                    repo_url_str
                ),
                candidates: vec![],
            };
        }
    };

    let step = Step::new(
        "curl",
        vec![
            "-fsSL",
            "-m",
            "10",
            "-H",
            "User-Agent: archbridge",
            &api_url,
        ],
        "Query GitHub release assets",
        10,
    );
    match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&out.stdout) {
                if let Some(assets) = val.get("assets").and_then(|a| a.as_array()) {
                    let mut candidates = Vec::new();
                    for asset in assets {
                        let name = asset.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let download_url = asset
                            .get("browser_download_url")
                            .and_then(|u| u.as_str())
                            .map(|s| s.to_string());

                        let matches = match asset_type {
                            "flatpak" => {
                                name.ends_with(".flatpakref") || name.ends_with(".flatpak")
                            }
                            "appimage" => {
                                name.ends_with(".AppImage") || name.ends_with(".appimage")
                            }
                            _ => false,
                        };

                        if matches {
                            candidates.push(Candidate {
                                name: name.to_string(),
                                version: val
                                    .get("tag_name")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string()),
                                url: download_url,
                                source_url: Some(repo_url_str.to_string()),
                                description: Some(format!("Release asset from {}", repo_url_str)),
                                ..Default::default()
                            });
                        }
                    }

                    if !candidates.is_empty() {
                        return DecisionRow {
                            source: asset_type.to_string(),
                            availability: "available".to_string(),
                            recommended: false,
                            reason: format!(
                                "Found {} {} release asset(s)",
                                candidates.len(),
                                asset_type
                            ),
                            candidates,
                        };
                    }
                }
            }
            DecisionRow {
                source: asset_type.to_string(),
                availability: "unavailable".to_string(),
                recommended: false,
                reason: format!(
                    "No {} asset found in latest release for repository",
                    asset_type
                ),
                candidates: vec![],
            }
        }
        Ok(_) => DecisionRow {
            source: asset_type.to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: format!("Could not fetch release metadata from {}", api_url),
            candidates: vec![],
        },
        Err(e) => DecisionRow {
            source: asset_type.to_string(),
            availability: "unknown".to_string(),
            recommended: false,
            reason: format!("Failed to query release API: {}", e),
            candidates: vec![],
        },
    }
}

fn query_upstream_source(target: &str, repo_url: Option<&str>) -> DecisionRow {
    let repo_url_str = match repo_url {
        Some(url) => url,
        None => {
            if target.starts_with("https://github.com/")
                || target.starts_with("https://gitlab.com/")
            {
                target
            } else {
                return query_github_repositories(target);
            }
        }
    };
    let mut notes: Vec<String> = Vec::new();

    if let Some((owner, repo)) = parse_github_repo(repo_url_str) {
        // A tagged release is the reproducible choice, so it is tried first; every
        // step that fails records why, instead of a later message guessing.
        let release = match github_api_json(&format!("/repos/{}/{}/releases/latest", owner, repo)) {
            Ok(val) => val
                .get("tag_name")
                .and_then(|t| t.as_str())
                .zip(val.get("tarball_url").and_then(|u| u.as_str()))
                .map(|(tag, url)| (tag.to_string(), url.to_string())),
            Err(err) => {
                notes.push(format!("no tagged release read: {}", err));
                None
            }
        };
        if let Some((tag_name, tarball_url)) = release {
            if probe_source_archive(&tarball_url).is_some() {
                return upstream_row(
                    &repo,
                    repo_url_str,
                    format!("GitHub source release for {}/{}", owner, repo),
                    Some(tag_name.trim_start_matches('v').to_string()),
                    Some(tag_name.clone()),
                    tarball_url,
                    format!("Verified GitHub source archive for tag '{}'", tag_name),
                );
            }
            notes.push(format!(
                "the archive advertised for tag '{}' could not be fetched",
                tag_name
            ));
        }

        // Repositories without releases are still buildable from a branch snapshot.
        let default_branch = match github_api_json(&format!("/repos/{}/{}", owner, repo)) {
            Ok(val) => match val.get("default_branch").and_then(|b| b.as_str()) {
                Some(branch) => Some(branch.to_string()),
                None => {
                    notes.push("the repository response named no default branch".to_string());
                    None
                }
            },
            Err(err) => {
                notes.push(format!("repository metadata read: {}", err));
                None
            }
        };
        if let Some(branch) = default_branch {
            let url = format!(
                "https://github.com/{}/{}/archive/refs/heads/{}.tar.gz",
                owner, repo, branch
            );
            if let Some(resolved) = probe_source_archive(&url) {
                return upstream_row(
                    &repo,
                    repo_url_str,
                    format!(
                        "GitHub source snapshot of branch '{}' for {}/{}",
                        branch, owner, repo
                    ),
                    revision_from_archive_url(&resolved),
                    Some(branch.clone()),
                    url,
                    format!("Using the '{}' branch source snapshot", branch),
                );
            }
            notes.push(format!(
                "the snapshot of branch '{}' could not be fetched",
                branch
            ));
        }

        // Unauthenticated API reads are rate limited per egress IP, and a rate
        // limit hides a branch that GitHub's own archive endpoint resolves as HEAD.
        let url = format!("https://github.com/{}/{}/archive/HEAD.tar.gz", owner, repo);
        if let Some(resolved) = probe_source_archive(&url) {
            let revision = revision_from_archive_url(&resolved);
            let shown = revision.clone().unwrap_or_else(|| "HEAD".to_string());
            return upstream_row(
                &repo,
                repo_url_str,
                format!("GitHub source snapshot of {}/{}", owner, repo),
                revision,
                Some("HEAD".to_string()),
                url,
                format!(
                    "Using the HEAD branch snapshot of '{}'; the release and branch listings were unreadable ({})",
                    shown,
                    notes.join("; ")
                ),
            );
        }
        notes.push("no branch snapshot was reachable".to_string());
        return upstream_unavailable(repo_url_str, notes);
    }

    if let Some(path) = parse_gitlab_repo(repo_url_str) {
        let project = path.split('/').last().unwrap_or_default().to_string();
        let url = format!(
            "https://gitlab.com/{}/-/archive/HEAD/{}-HEAD.tar.gz",
            path, project
        );
        if let Some(resolved) = probe_source_archive(&url) {
            let revision = revision_from_archive_url(&resolved);
            let shown = revision.clone().unwrap_or_else(|| "HEAD".to_string());
            return upstream_row(
                &project,
                repo_url_str,
                format!("GitLab source snapshot of {}", path),
                revision,
                Some("HEAD".to_string()),
                url,
                format!("Using the HEAD branch snapshot of '{}' ({})", path, shown),
            );
        }
        notes.push(format!(
            "no HEAD branch snapshot was reachable for '{}'",
            path
        ));
        return upstream_unavailable(repo_url_str, notes);
    }

    upstream_unavailable(
        repo_url_str,
        vec![format!(
            "'{}' is not a github.com or gitlab.com repository ArchBridge can resolve",
            repo_url_str
        )],
    )
}

/// One upstream candidate row: the archive URL is what `v1.prepare build` downloads.
fn upstream_row(
    name: &str,
    repo_url: &str,
    description: String,
    version: Option<String>,
    repository: Option<String>,
    url: String,
    reason: String,
) -> DecisionRow {
    DecisionRow {
        source: "upstream".to_string(),
        availability: "available".to_string(),
        recommended: false,
        reason,
        candidates: vec![Candidate {
            name: name.to_string(),
            version,
            url: Some(url),
            source_url: Some(repo_url.to_string()),
            description: Some(description),
            repository,
            ..Default::default()
        }],
    }
}

fn upstream_unavailable(repo_url: &str, notes: Vec<String>) -> DecisionRow {
    DecisionRow {
        source: "upstream".to_string(),
        availability: "unavailable".to_string(),
        recommended: false,
        reason: if notes.is_empty() {
            format!(
                "Could not resolve upstream release source for '{}'",
                repo_url
            )
        } else {
            format!(
                "Could not resolve upstream release source for '{}': {}",
                repo_url,
                notes.join("; ")
            )
        },
        candidates: vec![],
    }
}

/// Read a GitHub API path. Failure text is kept because the reason matters to the
/// caller: `curl -f` discards the body that explains a 403 rate limit.
fn github_api_json(path: &str) -> Result<serde_json::Value, String> {
    let api_url = format!("https://api.github.com{}", path);
    let step = Step::new(
        "curl",
        vec!["-sSL", "-m", "10", "-H", "User-Agent: archbridge", &api_url],
        "Query GitHub API",
        10,
    );
    let out = match run_step(&step, None) {
        Ok(out) => out,
        Err(err) => return Err(err),
    };
    if out.exit_code != 0 {
        let detail = out.stderr.trim();
        return Err(if detail.is_empty() {
            format!("request for {} failed", path)
        } else {
            format!("request for {} failed: {}", path, detail)
        });
    }
    let val: serde_json::Value = serde_json::from_str(out.stdout.trim())
        .map_err(|_| format!("{} did not return JSON", path))?;
    if let Some(message) = val.get("message").and_then(|m| m.as_str()) {
        return Err(format!("{} returned '{}'", path, message));
    }
    Ok(val)
}

/// Ask the URL itself whether it serves an archive, returning the address it
/// resolves to. A release listing can name an asset that is gone, and a 404 page
/// would otherwise reach the archive parser.
fn probe_source_archive(url: &str) -> Option<String> {
    let step = Step::new(
        "curl",
        vec![
            "-sSIL",
            "-m",
            "15",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code} %{content_type} %{url_effective}",
            url,
        ],
        "Verify upstream source archive",
        15,
    );
    match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 => match out.stdout.lines().last() {
            Some(final_response) => {
                let mut fields = final_response.split_whitespace();
                let is_archive = fields.next() == Some("200")
                    && !fields.next().unwrap_or("text/html").starts_with("text/");
                if is_archive {
                    fields
                        .next()
                        .map(str::to_string)
                        .or_else(|| Some(url.to_string()))
                } else {
                    None
                }
            }
            None => Some(url.to_string()),
        },
        _ => None,
    }
}

/// A pinned snapshot URL ends in the commit it contains, which is the only
/// version information a branch archive carries.
fn revision_from_archive_url(url: &str) -> Option<String> {
    let last = url.rsplit('/').next().filter(|s| !s.is_empty())?;
    (last.len() >= 7 && last.len() <= 40 && last.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| last.to_string())
}

/// The `group/project` path of a GitLab repository URL. Subgroups stay in the
/// path because that is how GitLab addresses projects; extra segments such as
/// `/-/tree/main` are rejected rather than folded into an archive URL.
pub fn parse_gitlab_repo(url: &str) -> Option<String> {
    let trimmed = url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |rest| rest.1);
    let segments: Vec<&str> = after_scheme.split('/').filter(|s| !s.is_empty()).collect();
    if segments.first().map(|h| h.to_lowercase()) != Some("gitlab.com".to_string()) {
        return None;
    }
    let path = &segments[1..];
    if path.len() < 2 || path.iter().any(|s| *s == "-" || s.contains(".git")) {
        return None;
    }
    Some(path.join("/"))
}

fn query_github_repositories(target: &str) -> DecisionRow {
    let query = target
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || c.is_ascii_whitespace())
        .collect::<String>()
        .split_whitespace()
        .map(encode_url_segment)
        .collect::<Vec<_>>()
        .join("+");
    if query.is_empty() {
        return DecisionRow {
            source: "upstream".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: "GitHub search requires a package name or repository URL".to_string(),
            candidates: vec![],
        };
    }

    let api_url = format!(
        "https://api.github.com/search/repositories?q={}&per_page=5",
        query
    );
    let step = Step::new(
        "curl",
        vec![
            "-fsSL",
            "-m",
            "10",
            "-H",
            "User-Agent: archbridge",
            &api_url,
        ],
        "Search GitHub repositories",
        10,
    );
    let output = match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 => out.stdout,
        _ => String::new(),
    };
    let value = match serde_json::from_str::<serde_json::Value>(&output) {
        Ok(value) => value,
        Err(_) => {
            return DecisionRow {
                source: "upstream".to_string(),
                availability: "unknown".to_string(),
                recommended: false,
                reason: "GitHub repository search returned no usable response".to_string(),
                candidates: vec![],
            };
        }
    };

    let normalized_target = target.to_ascii_lowercase().replace(['-', '_', ' '], "");
    let candidates = value
        .get("items")
        .and_then(|items| items.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?;
            let normalized_name = name.to_ascii_lowercase().replace(['-', '_', ' '], "");
            if !normalized_name.contains(&normalized_target)
                && !normalized_target.contains(&normalized_name)
            {
                return None;
            }
            let full_name = item.get("full_name")?.as_str()?;
            let branch = item
                .get("default_branch")
                .and_then(|branch| branch.as_str())
                .unwrap_or("main");
            let html_url = item.get("html_url")?.as_str()?.to_string();
            Some(Candidate {
                name: full_name.to_string(),
                version: None,
                url: Some(format!(
                    "https://github.com/{}/archive/refs/heads/{}.tar.gz",
                    full_name, branch
                )),
                source_url: Some(html_url),
                description: item
                    .get("description")
                    .and_then(|description| description.as_str())
                    .map(|description| description.to_string()),
                ..Default::default()
            })
        })
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        DecisionRow {
            source: "upstream".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: format!("No matching GitHub repository found for '{}'", target),
            candidates,
        }
    } else {
        DecisionRow {
            source: "upstream".to_string(),
            availability: "available".to_string(),
            recommended: false,
            reason: format!(
                "Found {} matching GitHub repository/repositories",
                candidates.len()
            ),
            candidates,
        }
    }
}

/// Extract `(owner, repo)` from a GitHub URL. The host must be the whole first
/// path-bearing segment: matching `github.com` anywhere in the string would
/// happily accept `https://notgithub.com.example/owner/repo`.
pub fn parse_github_repo(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |rest| rest.1);
    let mut segments = after_scheme.split('/').filter(|s| !s.is_empty());
    let host = segments.next()?.to_lowercase();
    if host != "github.com" {
        return None;
    }
    let owner = segments.next()?;
    let repo = segments.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_repo_requires_exact_host_segment() {
        assert_eq!(
            parse_github_repo("https://github.com/torvalds/linux"),
            Some(("torvalds".to_string(), "linux".to_string()))
        );
        assert_eq!(
            parse_github_repo("github.com/torvalds/linux.git/"),
            Some(("torvalds".to_string(), "linux".to_string()))
        );
        // A substring match let an attacker-controlled host through.
        assert_eq!(
            parse_github_repo("https://notgithub.com.example/torvalds/linux"),
            None
        );
        assert_eq!(
            parse_github_repo("https://evil.example/redirect?u=github.com/torvalds/linux"),
            None
        );
        assert_eq!(parse_github_repo("https://github.com/torvalds"), None);
    }

    #[test]
    fn gitlab_repo_requires_the_exact_host_and_a_project_path() {
        assert_eq!(
            parse_gitlab_repo("https://gitlab.com/gitlab-org/cli").as_deref(),
            Some("gitlab-org/cli")
        );
        assert_eq!(
            parse_gitlab_repo("https://gitlab.com/group/sub/project.git/").as_deref(),
            Some("group/sub/project")
        );
        // A URL naming a path inside the project is not the project itself.
        assert_eq!(
            parse_gitlab_repo("https://gitlab.com/gitlab-org/cli/-/archive/HEAD/cli-HEAD.tar.gz"),
            None
        );
        assert_eq!(parse_gitlab_repo("https://notgitlab.com.example/a/b"), None);
        assert_eq!(parse_gitlab_repo("https://gitlab.com/single"), None);
        assert_eq!(
            parse_gitlab_repo("https://evil.example/redirect?u=gitlab.com/a/b"),
            None
        );
    }

    #[test]
    fn only_a_commit_shaped_final_segment_counts_as_a_revision() {
        assert_eq!(
            revision_from_archive_url(
                "https://codeload.github.com/o/r/tar.gz/3fce3b5bb0236da2df6d99672afb8a719642eca7"
            )
            .as_deref(),
            Some("3fce3b5bb0236da2df6d99672afb8a719642eca7")
        );
        assert_eq!(
            revision_from_archive_url("https://github.com/o/r/archive/HEAD.tar.gz"),
            None
        );
        assert_eq!(
            revision_from_archive_url("https://gitlab.com/g/p/-/archive/HEAD/p-HEAD.tar.gz"),
            None
        );
    }

    #[test]
    fn url_encoding_cannot_shift_the_requested_resource() {
        assert_eq!(encode_url_segment("hello-world_1.2~3"), "hello-world_1.2~3");
        assert_eq!(encode_url_segment("a b"), "a%20b");
        // Each of these would otherwise terminate the path segment or add a query.
        let encoded = encode_url_segment("../../rpc?v=1#x&y=()");
        assert!(!encoded.contains(' '));
        assert!(!encoded.contains('?'));
        assert!(!encoded.contains('#'));
        assert!(!encoded.contains('&'));
        assert!(!encoded.contains('/'));
        assert_eq!(encoded, "..%2F..%2Frpc%3Fv%3D1%23x%26y%3D%28%29");
    }
}
